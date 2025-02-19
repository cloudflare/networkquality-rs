// Copyright (c) 2023-2024 Cloudflare, Inc.
// Licensed under the BSD-3-Clause license found in the LICENSE file or at https://opensource.org/licenses/BSD-3-Clause

mod load_generator;

use std::{
    collections::HashMap,
    fmt::{Debug, Display},
    future::Future,
    ops::Div,
    sync::Arc,
    time::Duration,
};

use humansize::{format_size, DECIMAL};
use nq_core::{
    client::{wait_for_finish, Direction, ThroughputClient},
    ConnectionType, Network, Time, Timestamp,
};
use nq_stats::{instant_minus_intervals, TimeSeries};
use tokio::{select, sync::mpsc};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, Instrument};
use url::Url;

pub use crate::load_generator::LoadedConnection;
use crate::load_generator::{LoadConfig, LoadGenerator};

#[derive(Debug, Clone)]
pub struct ResponsivenessConfig {
    pub large_download_url: Url,
    pub small_download_url: Url,
    pub upload_url: Url,
    pub moving_average_distance: usize,
    pub interval_duration: Duration,
    pub test_duration: Duration,
    pub trimmed_mean_percent: f64,
    pub std_tolerance: f64,
    pub max_loaded_connections: usize,
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
            large_download_url: "https://aim.cloudflare.com/responsiveness/api/v1/large"
                .parse()
                .unwrap(),
            small_download_url: "https://aim.cloudflare.com/cdn-cgi/build/nginx-ssl"
                .parse()
                .unwrap(),
            upload_url: "https://aim.cloudflare.com/responsiveness/api/v1/upload"
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

pub struct Responsiveness {
    start: Timestamp,
    config: ResponsivenessConfig,
    load_generator: LoadGenerator,
    foreign_probe_results: ForeignProbeResults,
    self_probe_results: SelfProbeResults,
    average_goodput_series: TimeSeries,
    rpm_series: TimeSeries,
    goodput_saturated: bool,
    rpm_saturated: bool,
    direction: Direction,
    rpm: f64,
    capacity: f64,
}

impl Responsiveness {
    pub fn new(config: ResponsivenessConfig, download: bool) -> anyhow::Result<Self> {
        let load_generator = LoadGenerator::new(config.load_config())?;

        Ok(Self {
            start: Timestamp::now(),
            config,
            load_generator,
            foreign_probe_results: Default::default(),
            self_probe_results: Default::default(),
            average_goodput_series: TimeSeries::new(),
            rpm_series: TimeSeries::new(),
            goodput_saturated: false,
            rpm_saturated: false,
            direction: if download {
                Direction::Down
            } else {
                Direction::Up(std::cmp::min(32u64 * 1024 * 1024 * 1024, usize::MAX as u64) as usize)
            },
            rpm: 0.0,
            capacity: 0.0,
        })
    }
}

impl Responsiveness {
    /// Run the responsiveness tests. This is a simple event loop which:
    /// - executes an interval of the RPM algorithm every `interval_duration`
    ///   seconds.
    /// - sends alternating self and foreign probes. todo(fisher): need to limit
    ///   to 100 probes/sec. (simple semaphore enough?).
    ///
    /// When the test completes or the test has been running too long, the test
    /// completes and the results are reported.
    pub async fn run_test(
        mut self,
        network: Arc<dyn Network>,
        time: Arc<dyn Time>,
        shutdown: CancellationToken,
    ) -> anyhow::Result<ResponsivenessResult> {
        let env = Env { time, network };
        self.start = env.time.now();

        info!("running responsiveness test: {:?}", self.config);

        let mut interval = None;

        // todo(fisher): switch to `Time` trait based sleep/interval impl to not
        // rely on tokio for rpm tests.
        let mut interval_timer = tokio::time::interval(self.config.interval_duration);

        let (event_tx, mut event_rx) = mpsc::channel(1024);

        self.new_load_generating_connection(event_tx.clone(), &env, shutdown.clone())?;
        self.send_foreign_probe(event_tx.clone(), &env, shutdown.clone())?;

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
                            if !self.send_self_probe(event_tx.clone(), &env, shutdown.clone())? {
                                self.send_foreign_probe(event_tx.clone(), &env, shutdown.clone())?;
                            }
                        }
                        Event::SelfProbe(s) => {
                            self.self_probe_results.add(s);

                            self.send_foreign_probe(event_tx.clone(), &env, shutdown.clone())?;
                        }
                        Event::Error(e) => {
                            error!("error: {e}");
                        }
                    }
                }
                _ = interval_timer.tick() => {
                    // updated the load generating connection state.
                    self.load_generator.update();

                    if let Some(interval) = interval.as_mut() {
                        if self.on_interval(*interval, event_tx.clone(), &env, shutdown.clone()).await? {
                            break;
                        }

                        *interval += 1;
                    } else {
                        interval = Some(0);
                    }
                }
                _ = shutdown.cancelled() => {
                    debug!("shutdown requested");
                    break;
                }
            };

            if env.time.now().duration_since(self.start) > self.config.test_duration {
                break;
            }
        }

        let now = env.time.now();
        if self.rpm == 0.0 {
            self.rpm = self
                .rpm_series
                .interval_average(now - Duration::from_secs(2), now)
                .unwrap_or(0.0);
        }

        // stop all on-going loads.
        let mut loads = self.load_generator.into_connections();
        loads.iter_mut().for_each(|load| load.stop());

        Ok(ResponsivenessResult {
            capacity: self.capacity,
            rpm: self.rpm,
            foreign_loaded_latencies: self.foreign_probe_results.http,
            self_probe_latencies: self.self_probe_results.http,
            loaded_connections: loads,
            duration: now.duration_since(self.start),
            average_goodput_series: self.average_goodput_series,
        })
    }

    /// Execute a single iteration of the responsiveness algorithm:
    ///
    /// * Create a load-generating connection.
    ///
    /// * At each interval:
    ///
    ///   - Create an additional load-generating connection.
    ///
    ///   - If goodput has not saturated:
    ///
    ///     - Compute the moving average aggregate goodput at interval i as
    ///       current_average.
    ///
    ///     - If the standard deviation of the past MAD average goodput values is less
    ///       than SDT of the current_average, declare goodput saturation and move on
    ///       to probe responsiveness.
    ///
    ///   - If goodput saturation has been declared:
    ///
    ///     - Compute the responsiveness at interval i as current_responsiveness.
    ///
    ///     - If the standard deviation of the past MAD responsiveness values is less
    ///       than SDT of the current_responsiveness, declare responsiveness
    ///       saturation and report current_responsiveness as the final test result.
    async fn on_interval(
        &mut self,
        interval: usize,
        event_tx: mpsc::Sender<Event>,
        env: &Env,
        shutdown: CancellationToken,
    ) -> anyhow::Result<bool> {
        // Determine the currently interval and round it to the interval duration.
        let end_data_interval = self.start + self.config.interval_duration * interval as u32;
        let start_data_interval = instant_minus_intervals(
            end_data_interval,
            self.config.moving_average_distance,
            self.config.interval_duration,
        );

        // always start a load generating connection
        // TODO: only if goodput is not saturated?
        if self.load_generator.count_loads() < self.config.max_loaded_connections
            && interval % 2 == 0
        {
            self.new_load_generating_connection(event_tx, env, shutdown)?;
        }

        let current_goodput = self.current_average_throughput(end_data_interval);
        self.average_goodput_series
            .add(end_data_interval, current_goodput);

        let std_goodput = self
            .average_goodput_series
            .interval_std(start_data_interval, end_data_interval)
            .unwrap_or(f64::MAX);

        // Goodput is saturated if the std of the last MAD goodputs is within
        // tolerance % of the current_average.
        let goodput_saturated = std_goodput < current_goodput * self.config.std_tolerance;
        if goodput_saturated {
            // Goodput has stabilized, set the capacity to the average
            // throughput of the last interval.
            self.capacity = current_goodput;
            self.goodput_saturated = true;
        }

        let current_rpm = compute_responsiveness(
            &self.foreign_probe_results,
            &self.self_probe_results,
            start_data_interval,
            end_data_interval,
            self.config.trimmed_mean_percent,
        )
        .unwrap_or(0.0);

        if current_rpm.is_nan() {
            panic!("NaN rpm!");
        }

        self.rpm_series.add(end_data_interval, current_rpm);

        let std_rpm = self
            .rpm_series
            .interval_std(start_data_interval, end_data_interval);

        let is_rpm_saturated = if let Some(std_rpm) = std_rpm {
            // RPM is saturated if the std of the last MAD RPMs is
            // within tolerance % of the current_rpm.
            if std_rpm < current_rpm * self.config.std_tolerance {
                self.rpm = current_rpm;
                self.rpm_saturated = true;
                true
            } else {
                false
            }
        } else {
            false
        };

        self.log_interval(
            interval,
            current_goodput,
            std_goodput,
            goodput_saturated,
            current_rpm,
            std_rpm,
            is_rpm_saturated,
        );

        // stop testing if both goodput and RPM saturated:
        Ok(self.goodput_saturated && self.rpm_saturated)
    }

    #[allow(clippy::too_many_arguments)]
    fn log_interval(
        &mut self,
        interval: usize,
        current_goodput: f64,
        std_goodput: f64,
        goodput_saturated: bool,
        current_rpm: f64,
        std_rpm: Option<f64>,
        is_rpm_saturated: bool,
    ) {
        // pretty print the results of the interval
        let custom_options = humansize::FormatSizeOptions::from(DECIMAL)
            .base_unit(humansize::BaseUnit::Bit)
            .long_units(false)
            .decimal_places(2);

        info!(
            interval,
            loads = self.load_generator.count_loads(),
            throughput = format_size(current_goodput as usize, custom_options),
            rpm = current_rpm,
            throughput_saturated = goodput_saturated,
            rpm_saturated = is_rpm_saturated,
            "interval finished"
        );

        info!(
            interval,
            throughput_std = format_size(std_goodput as usize, custom_options),
            throughput_target_std = format_size(
                (current_goodput * self.config.std_tolerance) as usize,
                custom_options
            ),
            rpm_std = std_rpm.unwrap_or(f64::NAN),
            rpm_target_std = current_rpm * self.config.std_tolerance,
            "interval stats"
        );
    }

    /// moving average aggregate goodput at interval p: The number of total
    /// bytes of data transferred within interval p and the MAD (Moving Average Distance) - 1 immediately
    /// preceding intervals, divided by MAD times ID (Interval Duration).
    ///
    /// https://datatracker.ietf.org/doc/html/draft-ietf-ippm-responsiveness-03#section-4.4-5.2.1
    fn current_average_throughput(&self, end_data_interval: Timestamp) -> f64 {
        let start_data_interval =
            instant_minus_intervals(end_data_interval, 4, self.config.interval_duration);

        let mut bytes_seen = 0.0;

        for connection in self.load_generator.connections() {
            bytes_seen += connection
                .total_bytes_series()
                .interval_sum(start_data_interval, end_data_interval);
        }

        let total_time = end_data_interval
            .duration_since(start_data_interval)
            .as_secs_f64();

        8.0 * bytes_seen / total_time
    }

    /// A GET/POST to an endpoint which sends/receives a large number of bytes
    /// as quickly as possible. The intent of these connections is to saturate
    /// a single connection's flow.
    #[tracing::instrument(skip_all)]
    fn new_load_generating_connection(
        &self,
        event_tx: mpsc::Sender<Event>,
        env: &Env,
        shutdown: CancellationToken,
    ) -> anyhow::Result<()> {
        let oneshot_res = self.load_generator.new_loaded_connection(
            self.direction,
            ConnectionType::H2,
            Arc::clone(&env.network),
            Arc::clone(&env.time),
            shutdown,
        )?;

        tokio::spawn(
            async move {
                let _ = match oneshot_res.await {
                    Ok(conn) => event_tx.send(Event::NewLoadedConnection(conn)),
                    Err(e) => event_tx.send(Event::Error(e)),
                }
                .await;
            }
            .in_current_span(),
        );

        Ok(())
    }

    /// Sends a foreign probe which is a GET on a newly created connection.
    ///
    /// > An HTTP GET request on a connection separate from the load-generating
    /// > connections ("foreign probes"). This probe type mimics the time it
    /// > takes for a web browser to connect to a new web server and request the
    /// > first element of a web page (e.g., "index.html"), or the startup time
    /// > for a video streaming client to launch and begin fetching media.
    ///
    /// https://datatracker.ietf.org/doc/html/draft-ietf-ippm-responsiveness-03#section-4.3-3.1.1
    fn send_foreign_probe(
        &mut self,
        event_tx: mpsc::Sender<Event>,
        env: &Env,
        shutdown: CancellationToken,
    ) -> anyhow::Result<()> {
        let inflight_body_fut = ThroughputClient::download()
            .new_connection(ConnectionType::H2)
            .send(
                self.config.small_download_url.as_str().parse()?,
                Arc::clone(&env.network),
                Arc::clone(&env.time),
                shutdown,
            )?;

        tokio::spawn(report_err(
            event_tx.clone(),
            async move {
                let inflight_body = inflight_body_fut.await?;

                let finished_result = wait_for_finish(inflight_body.events).await?;

                let Some(connection_timing) = inflight_body.timing else {
                    anyhow::bail!("a new connection with timing should have been created");
                };

                if event_tx
                    .send(Event::ForeignProbe(ForeignProbeResult {
                        start: connection_timing.start(),
                        time_connect: connection_timing.time_connect(),
                        time_secure: connection_timing.time_secure(),
                        time_body: finished_result
                            .finished_at
                            .duration_since(connection_timing.start()),
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

    /// Sends a self probe which is a GET on a load-generating connection.
    ///
    ///
    /// > An HTTP GET request multiplexed on the load-generating connections
    /// > ("self probes"). This probe type mimics the time it takes for a video
    /// > streaming client to skip ahead to a different chapter in the same
    /// > video stream, or for a navigation mapping application to react and
    /// > fetch new map tiles when the user scrolls the map to view a different
    /// > area. In a well functioning system, fetching new data over an existing
    /// > connection should take less time than creating a brand new TLS
    /// > connection from scratch to do the same thing.
    ///
    /// https://datatracker.ietf.org/doc/html/draft-ietf-ippm-responsiveness-03#section-4.3-3.2.1
    fn send_self_probe(
        &mut self,
        event_tx: mpsc::Sender<Event>,
        env: &Env,
        shutdown: CancellationToken,
    ) -> anyhow::Result<bool> {
        // The test client should uniformly and randomly select from the active
        // load-generating connections on which to send self probes.
        let Some(connection) = self.load_generator.random_connection() else {
            return Ok(false);
        };

        let inflight_body_fut = ThroughputClient::download()
            .with_connection(connection)
            .send(
                self.config.small_download_url.as_str().parse()?,
                Arc::clone(&env.network),
                Arc::clone(&env.time),
                shutdown,
            )?;

        tokio::spawn(report_err(
            event_tx.clone(),
            async move {
                let inflight_body = inflight_body_fut.await?;

                let finish_result = wait_for_finish(inflight_body.events).await?;
                debug!("self_probe_finished: {finish_result:?}");

                if event_tx
                    .send(Event::SelfProbe(SelfProbeResult {
                        start: inflight_body.start,
                        time_body: finish_result
                            .finished_at
                            .duration_since(inflight_body.start),
                    }))
                    .await
                    .is_err()
                {
                    anyhow::bail!("unable to send self probe result");
                }

                Ok(())
            }
            .in_current_span(),
        ));

        Ok(true)
    }
}

async fn report_err(event_tx: mpsc::Sender<Event>, f: impl Future<Output = anyhow::Result<()>>) {
    if let Err(e) = f.await {
        let _ = event_tx.send(Event::Error(e)).await;
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
        self.connect
            .add(result.start, result.time_connect.as_secs_f64() * 1000.0);
        self.secure
            .add(result.start, result.time_secure.as_secs_f64() * 1000.0);
        self.http
            .add(result.start, result.time_body.as_secs_f64() * 1000.0);
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
        self.http
            .add(result.start, result.time_body.as_secs_f64() * 1000.0);
    }

    pub fn http(&self) -> &TimeSeries {
        &self.http
    }
}

/// The responsiveness is then calculated as the weighted mean:
///
/// Responsiveness = 60000 /
/// (1/6*(TM(tcp_f) + TM(tls_f) + TM(http_f)) + 1/2*TM(http_s))
/// https://datatracker.ietf.org/doc/html/draft-ietf-ippm-responsiveness-03#section-4.3.1-4
fn compute_responsiveness(
    foreign_results: &ForeignProbeResults,
    self_results: &SelfProbeResults,
    from: Timestamp,
    to: Timestamp,
    percentile: f64,
) -> Option<f64> {
    let tm = |ts: &TimeSeries| ts.interval_trimmed_mean(from, to, percentile);

    let tcp_f = tm(foreign_results.connect())?;
    let tls_f = tm(foreign_results.secure())?;
    let http_f = tm(foreign_results.http())?;
    let http_s = tm(self_results.http())?;

    let foreign_sum = tcp_f + tls_f + http_f;

    Some(60_000.0 / (foreign_sum.div(6.0) + http_s.div(2.0)))
}

#[derive(Debug)]
pub struct ForeignProbeResult {
    start: Timestamp,
    time_connect: Duration,
    time_secure: Duration,
    time_body: Duration,
}

#[derive(Debug)]
pub struct SelfProbeResult {
    start: Timestamp,
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

#[derive(Clone)]
struct Env {
    time: Arc<dyn Time>,
    network: Arc<dyn Network>,
}

#[derive(Default, Debug)]
pub struct ResponsivenessResult {
    pub duration: Duration,
    pub capacity: f64,
    pub rpm: f64,
    pub foreign_loaded_latencies: TimeSeries,
    pub self_probe_latencies: TimeSeries,
    pub loaded_connections: Vec<LoadedConnection>,
    pub average_goodput_series: TimeSeries,
}

impl ResponsivenessResult {
    pub fn throughput(&self) -> Option<usize> {
        self.average_goodput_series
            .quantile(0.90)
            .map(|t| t as usize)
    }
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
