//! Independent, bounded ingress lanes keep anonymous delivery/enrollment work
//! from occupying management and revocation capacity. Source is the TCP peer.
use axum::{
    extract::{ConnectInfo, Request},
    middleware::Next,
    response::Response,
};
use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr},
    sync::{Arc, Mutex},
    time::Instant,
};
use tokio::sync::Semaphore;

struct Sources {
    last_sweep: Instant,
    entries: HashMap<IpAddr, (Instant, u32)>,
}

#[derive(Clone)]
pub(crate) struct Lane {
    work: Arc<Semaphore>,
    sources: Arc<Mutex<Sources>>,
    per_minute: u32,
}
impl Lane {
    pub(crate) fn new(concurrent: usize, per_minute: u32) -> Self {
        Self {
            work: Arc::new(Semaphore::new(concurrent)),
            sources: Arc::new(Mutex::new(Sources {
                last_sweep: Instant::now(),
                entries: HashMap::new(),
            })),
            per_minute,
        }
    }
    pub(crate) async fn admit(self, request: Request, next: Next) -> Response {
        let source = request
            .extensions()
            .get::<ConnectInfo<SocketAddr>>()
            .map(|v| v.0.ip());
        #[cfg(test)]
        let source = source.or(Some(IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)));
        let Some(source) = source else {
            return super::http::throttled(60);
        };
        if !self.allow_source(source) {
            return super::http::throttled(60);
        }
        let Ok(_permit) = self.work.try_acquire() else {
            return super::http::throttled(1);
        };
        next.run(request).await
    }
    fn allow_source(&self, source: IpAddr) -> bool {
        let Ok(mut sources) = self.sources.lock() else {
            return false;
        };
        let now = Instant::now();
        if now.duration_since(sources.last_sweep).as_secs() >= 60 {
            sources
                .entries
                .retain(|_, (start, _)| now.duration_since(*start).as_secs() < 60);
            sources.last_sweep = now;
        }
        if sources.entries.len() >= 4096 && !sources.entries.contains_key(&source) {
            return false;
        }
        let (start, used) = sources.entries.entry(source).or_insert((now, 0));
        if now.duration_since(*start).as_secs() >= 60 {
            *start = now;
            *used = 0;
        }
        if *used >= self.per_minute {
            return false;
        }
        *used += 1;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bookkeeping_and_sources_are_bounded_and_lanes_are_independent() {
        let delivery = Lane::new(8, 1);
        let revocation = Lane::new(4, 1);
        for n in 0..4096u32 {
            assert!(delivery.allow_source(IpAddr::V4(n.into())));
        }
        let other = IpAddr::V4(5000u32.into());
        assert!(!delivery.allow_source(other));
        assert!(revocation.allow_source(other));
        assert!(!revocation.allow_source(other));
        assert_eq!(delivery.sources.lock().unwrap().entries.len(), 4096);
    }
    #[tokio::test]
    async fn delivery_cannot_acquire_reserved_revocation_work() {
        let delivery = Lane::new(8, 10);
        let revocation = Lane::new(4, 10);
        let _busy = delivery.work.acquire_many(8).await.unwrap();
        assert!(delivery.work.try_acquire().is_err());
        assert!(revocation.work.try_acquire().is_ok());
    }
}
