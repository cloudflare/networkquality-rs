use std::{
    collections::HashMap,
    fmt::{Debug, Display},
    future::Future,
    ops::Div,
    pin::Pin,
    sync::Arc,
    time::Duration,
};

use humansize::{format_size, DECIMAL};
use nq_io::{
    client::{wait_for_finish, Direction},
    Network,
};
use nq_stats::{instant_minus_intervals, TimeSeries};
use tokio::{
    select,
    sync::{mpsc, oneshot},
    time::Instant,
};
use tracing::{error, Instrument};
use url::Url;

use crate::{load::LoadedConnection, LoadConfig, LoadGenerator, Speedtest};

pub struct ResponsivenessConfig {
    large_download_url: Url,
    small_download_url: Url,
    upload_url: Url,
    moving_average_distance: usize,
    interval_duration: Duration,
    test_duration: Duration,
    trimmed_mean_percent: f64,
    std_tolerance: f64,
    max_loaded_connections: usize,
}

impl ResponsivenessConfig {
    pub fn load_config(&self) -> LoadConfig {
        LoadConfig {
            headers: HashMap::default(),
            download_url: self.large_download_url.clone(),
            upload_url: self.upload_url.clone(),
            upload_size: 4_000_000_000, // 4 GB
        }
    }
}

impl Default for ResponsivenessConfig {
    fn default() -> Self {
        Self {
            large_download_url: "https://mensura.cdn-apple.com/api/v1/seed/large"
                .parse()
                .unwrap(),
            small_download_url: "https://mensura.cdn-apple.com/api/v1/seed/small"
                .parse()
                .unwrap(),
            upload_url: "https://mensura.cdn-apple.com/api/v1/seed/slurp"
                .parse()
                .unwrap(),
            moving_average_distance: 4,
            interval_duration: Duration::from_millis(1000),
            test_duration: Duration::from_secs(20),
            trimmed_mean_percent: 0.95,
            std_tolerance: 0.05,
            max_loaded_connections: 16,
        }
    }
}

#[derive(Default, Debug)]
pub struct ResponsivenessResult {
    capacity: f64,
    rpm: f64,
}

impl Display for ResponsivenessResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let custom_options = humansize::FormatSizeOptions::from(DECIMAL)
            .base_unit(humansize::BaseUnit::Bit)
            .long_units(false)
            .decimal_places(2);
        writeln!(
            f,
            "{:8}: {}/s",
            "capacity",
            format_size(self.capacity as usize, custom_options)
        )?;
        write!(f, "{:>8}: {}", "rpm", self.rpm.round() as usize)
    }
}

pub struct Responsiveness {
    start: Instant,
    config: ResponsivenessConfig,
    load_generator: LoadGenerator,
    foreign_probe_results: ForeignProbeResults,
    self_probe_results: SelfProbeResults,
    average_goodput_series: TimeSeries,
    rpm_series: TimeSeries,
    goodput_saturated: bool,
    rpm_saturated: bool,
    result: ResponsivenessResult,
}

impl Responsiveness {
    pub fn new(config: ResponsivenessConfig) -> anyhow::Result<Self> {
        let load_generator = LoadGenerator::new(config.load_config())?;

        Ok(Self {
            start: Instant::now(),
            config,
            load_generator,
            foreign_probe_results: Default::default(),
            self_probe_results: Default::default(),
            average_goodput_series: TimeSeries::new(),
            rpm_series: TimeSeries::new(),
            goodput_saturated: false,
            rpm_saturated: false,
            result: ResponsivenessResult::default(),
        })
    }
}

impl Responsiveness {
    async fn run(
        mut self,
        network: Arc<dyn Network>,
        _shutdown: oneshot::Receiver<()>,
    ) -> anyhow::Result<ResponsivenessResult> {
        self.start = Instant::now();

        let mut interval = 0;
        let mut interval_timer = tokio::time::interval(self.config.interval_duration);

        let (event_tx, mut event_rx) = mpsc::channel(1024);

        self.new_load_generating_connection(Arc::clone(&network), event_tx.clone())?;
        self.send_foreign_probe(Arc::clone(&network), event_tx.clone())?;

        loop {
            select! {
                Some(event) = event_rx.recv() => {
                    match event {
                        Event::NewLoadedConnection(connection) => {
                            self.load_generator.push(connection);
                        }
                        Event::ForeignProbe(f) => {
                            self.foreign_probe_results.add(f);

                            // There might not be an available load generating
                            // connection to send a self probe on. If that's the
                            // case, send another foreign probe.
                            if !self.send_self_probe(Arc::clone(&network), event_tx.clone())? {
                                self.send_foreign_probe(Arc::clone(&network), event_tx.clone())?;
                            }
                        }
                        Event::SelfProbe(s) => {
                            self.self_probe_results.add(s);
                            self.send_foreign_probe(Arc::clone(&network), event_tx.clone())?;
                        }
                        Event::Error(e) => {
                            println!("error: {e}");
                        }
                    }
                }
                _ = interval_timer.tick() => {
                    self.load_generator.update();

                    if self.on_interval(Arc::clone(&network), interval, event_tx.clone()).await? {
                        break;
                    }

                    interval += 1;
                }
            };

            if self.start.elapsed() > self.config.test_duration {
                break;
            }
        }

        let now = Instant::now();
        if self.result.rpm == 0.0 {
            self.result.rpm = self
                .rpm_series
                .interval_average((now - Duration::from_secs(2)).into_std(), now.into_std())
                .unwrap_or(0.0);
        }

        println!("\n{}", self.result);

        Ok(self.result)
    }

    async fn on_interval(
        &mut self,
        network: Arc<dyn Network>,
        interval: usize,
        event_tx: mpsc::Sender<Event>,
    ) -> anyhow::Result<bool> {
        println!(
            "interval: {interval}, loads={}",
            self.load_generator.count_loads()
        );

        let end_data_interval = self.start + self.config.interval_duration * interval as u32;
        let start_data_interval = instant_minus_intervals(
            end_data_interval.into_std(),
            self.config.moving_average_distance,
            self.config.interval_duration,
        );

        // always start a load generating connection
        // TODO: only if goodput is not saturated?
        if self.load_generator.count_loads() < self.config.max_loaded_connections {
            self.new_load_generating_connection(Arc::clone(&network), event_tx)?;
        }

        let current_goodput = self.current_average_throughput(end_data_interval);
        self.average_goodput_series
            .add(end_data_interval.into_std(), current_goodput);

        let std_goodput = self
            .average_goodput_series
            .interval_std(start_data_interval, end_data_interval.into_std())
            .unwrap_or(std::f64::MAX);

        // Goodput is saturated if the std of the last MAD goodputs
        // is within tolerance % of the current_average.
        let goodput_saturated = std_goodput < current_goodput * self.config.std_tolerance;
        if goodput_saturated {
            // Goodput has stabilized, set the capacity to the average
            // throughput of the last interval.
            self.result.capacity = current_goodput;
            self.goodput_saturated = true;
        }

        let custom_options = humansize::FormatSizeOptions::from(DECIMAL)
            .base_unit(humansize::BaseUnit::Bit)
            .long_units(false)
            .decimal_places(2);
        println!(
            "\tthroughput: {}/s σ{}/s, target σ: {}/s, saturated: {}",
            format_size(current_goodput as usize, custom_options),
            format_size(std_goodput as usize, custom_options),
            format_size(
                (current_goodput * self.config.std_tolerance) as usize,
                custom_options
            ),
            goodput_saturated,
        );

        let current_rpm = compute_responsiveness(
            &self.foreign_probe_results,
            &self.self_probe_results,
            start_data_interval.into(),
            end_data_interval,
            self.config.trimmed_mean_percent,
        )
        .unwrap_or(0.0);
        self.rpm_series
            .add(end_data_interval.into_std(), current_rpm);

        let std_rpm = self
            .rpm_series
            .interval_std(start_data_interval, end_data_interval.into_std());

        let rpm_saturated = if let Some(std_rpm) = std_rpm {
            if std_rpm < current_rpm * self.config.std_tolerance {
                self.result.rpm = current_rpm;
                self.rpm_saturated = true;
                true
            } else {
                false
            }
        } else {
            false
        };

        println!(
            "\trpm: {:.2} σ{:.2}, target: {:.2}, saturated: {}",
            current_rpm,
            std_rpm.unwrap_or(f64::NAN),
            current_rpm * self.config.std_tolerance,
            rpm_saturated
        );

        println!();
        Ok(self.goodput_saturated && self.rpm_saturated)
    }

    fn current_average_throughput(&self, end_data_interval: Instant) -> f64 {
        let start_data_interval = instant_minus_intervals(
            end_data_interval.into_std(),
            4,
            self.config.interval_duration,
        );
        let mut bytes_seen = 0.0;

        for connection in self.load_generator.connections() {
            bytes_seen += connection
                .total_bytes_series()
                .interval_sum(start_data_interval, end_data_interval.into_std());
        }

        let total_time = end_data_interval
            .duration_since(start_data_interval.into())
            .as_secs_f64();

        8.0 * bytes_seen / total_time
    }

    #[tracing::instrument(skip_all)]
    fn new_load_generating_connection(
        &self,
        network: Arc<dyn Network>,
        event_tx: mpsc::Sender<Event>,
    ) -> anyhow::Result<()> {
        let oneshot_res = self.load_generator.new_loaded_connection(
            network,
            Direction::Down,
            nq_io::ConnectionType::H2,
        )?;

        tokio::spawn(
            async move {
                let send_res = match oneshot_res.await {
                    Ok(Ok(conn)) => event_tx.send(Event::NewLoadedConnection(conn)),
                    Ok(Err(e)) => event_tx.send(Event::Error(e)),
                    Err(_) => {
                        error!("error receiving load generating connection");
                        return;
                    }
                }
                .await;

                if send_res.is_err() {
                    error!("error sending load generating connection to event stream");
                }
            }
            .in_current_span(),
        );

        Ok(())
    }

    fn send_foreign_probe(
        &mut self,
        network: Arc<dyn Network>,
        event_tx: mpsc::Sender<Event>,
    ) -> anyhow::Result<()> {
        let inflight_body_fut = nq_io::client::ThroughputClient::default()
            .new_connection(nq_io::ConnectionType::H2)
            .send(
                network,
                Direction::Down,
                self.config.small_download_url.as_str().parse()?,
            )?;

        tokio::spawn(report_err(
            event_tx.clone(),
            async move {
                let inflight_body = inflight_body_fut.await??;

                let finished_at = wait_for_finish(inflight_body.events).await?;

                let Some(connection_timing) = inflight_body.connection_timing else {
                anyhow::bail!("a new connection with timing should have been created");
            };

                if event_tx
                    .send(Event::ForeignProbe(ForeignProbeResult {
                        start: connection_timing.started_at,
                        time_connect: connection_timing.time_connect,
                        time_secure: connection_timing.time_secure,
                        time_body: finished_at.duration_since(connection_timing.started_at),
                    }))
                    .await
                    .is_err()
                {
                    anyhow::bail!("unable to send foreign probe result");
                }

                Ok(())
            }
            .in_current_span(),
        ));

        Ok(())
    }

    fn send_self_probe(
        &mut self,
        network: Arc<dyn Network>,
        event_tx: mpsc::Sender<Event>,
    ) -> anyhow::Result<bool> {
        let Some(conn_id) = self.load_generator.random_connection() else {
            return Ok(false)
        };

        let inflight_body_fut = nq_io::client::ThroughputClient::default()
            .with_connection(conn_id)
            .send(
                network,
                Direction::Down,
                self.config.small_download_url.as_str().parse()?,
            )?;

        tokio::spawn(report_err(event_tx.clone(), async move {
            let inflight_body = inflight_body_fut.await??;

            let finished_at = wait_for_finish(inflight_body.events).await?;

            if event_tx
                .send(Event::SelfProbe(SelfProbeResult {
                    start: inflight_body.start,
                    time_body: finished_at.duration_since(inflight_body.start),
                }))
                .await
                .is_err()
            {
                anyhow::bail!("unable to send self probe result");
            }

            Ok(())
        }));

        Ok(true)
    }
}

async fn report_err(event_tx: mpsc::Sender<Event>, f: impl Future<Output = anyhow::Result<()>>) {
    if let Err(e) = f.await {
        let _ = event_tx.send(Event::Error(e)).await;
    }
}

impl<N> Speedtest<N> for Responsiveness
where
    N: Network,
{
    type TestResult = ResponsivenessResult;

    fn run(
        self,
        network: N,
        shutdown: oneshot::Receiver<()>,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<ResponsivenessResult>> + Send + 'static>> {
        let network = Arc::new(network);
        Box::pin(Responsiveness::run(self, network, shutdown))
    }
}

#[derive(Default)]
pub struct ForeignProbeResults {
    connect: TimeSeries,
    secure: TimeSeries,
    http: TimeSeries,
}

impl ForeignProbeResults {
    pub fn add(&mut self, result: ForeignProbeResult) {
        self.connect.add(
            result.start.into_std(),
            result.time_connect.as_secs_f64() * 1000.0,
        );
        self.secure.add(
            result.start.into_std(),
            result.time_secure.as_secs_f64() * 1000.0,
        );
        self.http.add(
            result.start.into_std(),
            result.time_body.as_secs_f64() * 1000.0,
        );
    }

    pub fn connect(&self) -> &TimeSeries {
        &self.connect
    }

    pub fn secure(&self) -> &TimeSeries {
        &self.secure
    }

    pub fn http(&self) -> &TimeSeries {
        &self.http
    }
}

#[derive(Default)]
pub struct SelfProbeResults {
    http: TimeSeries,
}

impl SelfProbeResults {
    pub fn add(&mut self, result: SelfProbeResult) {
        self.http.add(
            result.start.into_std(),
            result.time_body.as_secs_f64() * 1000.0,
        );
    }

    pub fn http(&self) -> &TimeSeries {
        &self.http
    }
}

fn compute_responsiveness(
    foreign_results: &ForeignProbeResults,
    self_results: &SelfProbeResults,
    from: Instant,
    to: Instant,
    percentile: f64,
) -> Option<f64> {
    let tm = |ts: &TimeSeries| ts.interval_trimmed_mean(from.into_std(), to.into_std(), percentile);

    let tcp_f = tm(foreign_results.connect())?;
    let tls_f = tm(foreign_results.secure())?;
    let http_f = tm(foreign_results.http())?;
    let http_s = tm(self_results.http())?;

    let foreign_sum = tcp_f + tls_f + http_f;

    Some(60_000.0 / (foreign_sum.div(6.0) + http_s.div(2.0)))
}

#[derive(Debug)]
pub struct ForeignProbeResult {
    start: Instant,
    time_connect: Duration,
    time_secure: Duration,
    time_body: Duration,
}

#[derive(Debug)]
pub struct SelfProbeResult {
    start: Instant,
    time_body: Duration,
}

enum Event {
    ForeignProbe(ForeignProbeResult),
    SelfProbe(SelfProbeResult),
    NewLoadedConnection(LoadedConnection),
    Error(anyhow::Error),
}

impl Debug for Event {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ForeignProbe(_) => f.debug_tuple("ForeignProbe").finish(),
            Self::SelfProbe(_) => f.debug_tuple("SelfProbe").finish(),
            Self::NewLoadedConnection(_) => f.debug_tuple("NewLoadedConnection").finish(),
            Self::Error(_) => f.debug_tuple("Error").finish(),
        }
    }
}
