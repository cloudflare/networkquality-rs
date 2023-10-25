use std::{collections::HashMap, sync::Arc, time::Duration};

use http::{HeaderMap, HeaderName, HeaderValue};
use nq_core::client::{Direction, ThroughputClient};
use nq_core::{
    oneshot_result, BodyEvent, ConnectionId, ConnectionType, Network, OneshotResult, Time,
    Timestamp,
};
use nq_stats::CounterSeries;
use rand::seq::SliceRandom;
use serde::Deserialize;
use tokio::sync::mpsc::Receiver;
use tracing::Instrument;

#[derive(Debug, Deserialize)]
pub struct LoadConfig {
    pub headers: HashMap<String, String>,
    pub download_url: url::Url,
    pub upload_url: url::Url,
    pub upload_size: usize,
}

pub struct LoadGenerator {
    headers: HeaderMap<HeaderValue>,
    config: LoadConfig,
    loads: Vec<LoadedConnection>,
}

impl LoadGenerator {
    pub fn new(config: LoadConfig) -> anyhow::Result<Self> {
        let mut headers = HeaderMap::new();

        for (key, value) in config.headers.iter() {
            headers.insert(
                HeaderName::from_bytes(key.as_bytes())?,
                HeaderValue::from_bytes(value.as_bytes())?,
            );
        }

        Ok(Self {
            headers,
            config,
            loads: Vec::new(),
        })
    }

    #[tracing::instrument(skip(self, network, time))]
    pub fn new_loaded_connection(
        &self,
        direction: Direction,
        conn_type: ConnectionType,
        network: Arc<dyn Network>,
        time: Arc<dyn Time>,
    ) -> anyhow::Result<OneshotResult<LoadedConnection>> {
        let (tx, rx) = oneshot_result();

        let client = match direction {
            Direction::Down => ThroughputClient::download(),
            Direction::Up(size) => ThroughputClient::upload(size),
        };

        let response_fut = client
            .new_connection(conn_type)
            .headers(self.headers.clone())
            .send(
                match direction {
                    Direction::Up(_) => self.config.upload_url.as_str().parse()?,
                    Direction::Down => self.config.download_url.as_str().parse()?,
                },
                network,
                time,
            )?;

        tokio::spawn(
            async move {
                let inflight_body = response_fut.await?;

                let _ = tx.send(Ok(LoadedConnection {
                    conn_id: inflight_body.conn_id,
                    events_rx: inflight_body.events,
                    total_bytes_series: CounterSeries::new(),
                    finished_at: None,
                }));

                Ok::<_, anyhow::Error>(())
            }
            .in_current_span(),
        );

        Ok(rx)
    }

    pub fn connections(&self) -> impl Iterator<Item = &LoadedConnection> {
        self.loads.iter()
    }

    pub fn random_connection(&self) -> Option<ConnectionId> {
        self.loads
            .choose(&mut rand::thread_rng())
            .map(|c| c.conn_id)
    }

    pub fn push(&mut self, loaded_connection: LoadedConnection) {
        self.loads.push(loaded_connection);
    }

    pub fn update(&mut self) {
        for load in &mut self.loads {
            load.update();
        }
    }

    pub fn count_loads(&self) -> usize {
        self.loads.len()
    }
}

#[derive(Debug)]
pub struct LoadedConnection {
    conn_id: ConnectionId,
    events_rx: Receiver<BodyEvent>,
    total_bytes_series: CounterSeries,
    finished_at: Option<Timestamp>,
}

impl LoadedConnection {
    pub fn update(&mut self) {
        while let Ok(event) = self.events_rx.try_recv() {
            match event {
                BodyEvent::ByteCount { at, total } => self.total_bytes_series.add(at, total as f64),
                BodyEvent::Finished { at } => self.finished_at = Some(at),
            }
        }
    }

    pub fn total_bytes_series(&self) -> &CounterSeries {
        &self.total_bytes_series
    }
}

#[derive(Debug)]
pub struct LoadTestResult {
    pub total_bytes: usize,
    pub total_time: Duration,
}
