//! HTTP plumbing for the API client: typed errors, retry decisions, a request gate and
//! cancellation. Decisions are plain functions so failures can be tested without a network.

use std::{
    fmt,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

/// Attempts for one logical request, including the first.
pub const MAX_ATTEMPTS: u32 = 4;
/// All attempts and waits of one logical request must fit in this.
pub const REQUEST_DEADLINE: Duration = Duration::from_secs(45);
/// One attempt, from connecting to the last byte.
pub const ATTEMPT_TIMEOUT: Duration = Duration::from_secs(20);
/// Rate limits longer than this fail visibly instead of freezing the song.
pub const MAX_RETRY_AFTER: Duration = Duration::from_secs(30);
/// Requests running at once, so preloading cannot crowd out the song you asked for.
pub const MAX_CONCURRENT: usize = 4;
/// A rejected client id is only re-scraped this often, whatever the callers do.
pub const CLIENT_ID_REFRESH_EVERY: Duration = Duration::from_secs(300);

const BACKOFF_BASE: Duration = Duration::from_millis(400);
const BACKOFF_MAX: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, PartialEq)]
pub enum ApiError {
    Timeout,
    Network,
    RateLimited,
    /// The OAuth token was refused. Only `scc login` fixes this.
    Auth,
    /// The request was understood and refused: geo-blocked, or private to someone else.
    Forbidden,
    Unavailable,
    /// A signed link or stream authorization ran out.
    ExpiredStream,
    /// Only encrypted streams exist, which scc does not play.
    Protected,
    Server(u16),
    ClientId,
    Rejected(u16),
    Invalid,
    Cancelled,
}

impl ApiError {
    /// Worth trying again later. Everything else stays failed until something changes.
    pub fn is_transient(&self) -> bool {
        matches!(
            self,
            ApiError::Timeout | ApiError::Network | ApiError::RateLimited | ApiError::Server(_)
        )
    }
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ApiError::Timeout => write!(f, "SoundCloud did not answer in time"),
            ApiError::Network => write!(f, "could not reach SoundCloud"),
            ApiError::RateLimited => write!(f, "SoundCloud is limiting requests, try again shortly"),
            ApiError::Auth => write!(f, "SoundCloud rejected your login, run `scc login`"),
            ApiError::Forbidden => write!(f, "not available to this account or region"),
            ApiError::Unavailable => write!(f, "no longer available on SoundCloud"),
            ApiError::ExpiredStream => write!(f, "the stream link expired"),
            ApiError::Protected => write!(f, "this song is DRM protected"),
            ApiError::Server(c) => write!(f, "SoundCloud had a problem (HTTP {c})"),
            ApiError::ClientId => write!(f, "could not get a SoundCloud client id"),
            ApiError::Rejected(c) => write!(f, "SoundCloud refused the request (HTTP {c})"),
            ApiError::Invalid => write!(f, "SoundCloud sent an unreadable answer"),
            ApiError::Cancelled => write!(f, "cancelled"),
        }
    }
}

impl std::error::Error for ApiError {}

#[derive(Debug, PartialEq)]
pub enum Step {
    Done,
    Fail(ApiError),
    RefreshClientId,
    /// Wait and try again; `Some` is the server's own Retry-After.
    Retry(Option<Duration>),
}

/// What the caller knows when a status comes back.
#[derive(Clone, Copy)]
pub struct Seen {
    pub has_token: bool,
    /// A request already succeeded with this client id, so a refusal is about something else.
    pub id_proven: bool,
    /// This request has not refreshed the client id yet.
    pub may_refresh: bool,
}

/// 401 can mean an old client id or a bad OAuth token, so it earns one refresh before it is
/// blamed on the login. 403 is a real refusal unless the client id was never shown to work.
pub fn next_step(status: u16, retry_after: Option<Duration>, s: Seen) -> Step {
    match status {
        200..=299 => Step::Done,
        401 if s.may_refresh => Step::RefreshClientId,
        401 if s.has_token => Step::Fail(ApiError::Auth),
        401 => Step::Fail(ApiError::ClientId),
        403 if s.may_refresh && !s.id_proven => Step::RefreshClientId,
        403 => Step::Fail(ApiError::Forbidden),
        404 | 410 => Step::Fail(ApiError::Unavailable),
        429 => match retry_after {
            Some(d) if d > MAX_RETRY_AFTER => Step::Fail(ApiError::RateLimited),
            d => Step::Retry(d),
        },
        408 | 500..=599 => Step::Retry(None),
        c => Step::Fail(ApiError::Rejected(c)),
    }
}

/// Doubling delay with jitter in its upper half, so retrying clients do not move in step.
pub fn backoff(attempt: u32, seed: u64) -> Duration {
    let ceiling = BACKOFF_BASE
        .saturating_mul(1u32 << attempt.min(10))
        .min(BACKOFF_MAX)
        .as_millis() as u64;
    let mut x = seed | 1;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    Duration::from_millis(ceiling / 2 + x % (ceiling / 2 + 1))
}

/// Retry-After accepts either delay-seconds or an IMF-fixdate in GMT.
pub fn retry_after(value: &str, now: std::time::SystemTime) -> Option<Duration> {
    let value = value.trim();
    if !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit()) {
        return value.parse().ok().map(Duration::from_secs);
    }
    let fields: Vec<_> = value.split_whitespace().collect();
    if fields.len() != 6
        || fields[5] != "GMT"
        || !matches!(fields[0], "Mon," | "Tue," | "Wed," | "Thu," | "Fri," | "Sat," | "Sun,")
    {
        return None;
    }
    let day: i64 = fields[1].parse().ok()?;
    let month = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ]
    .iter()
    .position(|m| *m == fields[2])?
        + 1;
    let year: i64 = fields[3].parse().ok()?;
    if !(1970..=9999).contains(&year) {
        return None;
    }
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days = [31, if leap { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    if day < 1 || day > days[month - 1] {
        return None;
    }
    let time: Vec<u64> = fields[4].split(':').map(str::parse).collect::<Result<_, _>>().ok()?;
    if time.len() != 3 || time[0] > 23 || time[1] > 59 || time[2] > 59 {
        return None;
    }
    let before = |y: i64| 365 * y + y / 4 - y / 100 + y / 400;
    let elapsed_days = before(year - 1) - before(1969) + days[..month - 1].iter().sum::<i64>() + day - 1;
    let seconds = elapsed_days as u64 * 86400 + time[0] * 3600 + time[1] * 60 + time[2];
    let date = std::time::UNIX_EPOCH.checked_add(Duration::from_secs(seconds))?;
    Some(date.duration_since(now).unwrap_or(Duration::ZERO))
}

/// Check cancellation and the shared deadline before starting another blocking operation.
pub fn remaining(deadline: Instant, cancel: &Cancel) -> Result<Duration, ApiError> {
    if cancel.is_stale() {
        return Err(ApiError::Cancelled);
    }
    deadline
        .checked_duration_since(Instant::now())
        .filter(|d| !d.is_zero())
        .ok_or(ApiError::Timeout)
}

/// Limits how many requests run at once.
pub struct Gate {
    free: Mutex<usize>,
    freed: Condvar,
}

pub struct Permit<'a>(&'a Gate);

impl Gate {
    pub fn new(n: usize) -> Self {
        Gate {
            free: Mutex::new(n),
            freed: Condvar::new(),
        }
    }

    pub fn enter(&self, deadline: Instant, cancel: &Cancel) -> Result<Permit<'_>, ApiError> {
        let mut free = self.free.lock().unwrap();
        loop {
            let left = remaining(deadline, cancel)?;
            if *free > 0 {
                *free -= 1;
                return Ok(Permit(self));
            }
            (free, _) = self
                .freed
                .wait_timeout(free, left.min(Duration::from_millis(100)))
                .unwrap();
        }
    }
}

impl Drop for Permit<'_> {
    fn drop(&mut self) {
        *self.0.free.lock().unwrap() += 1;
        self.0.freed.notify_one();
    }
}

/// Lets a queued or retrying request notice that the song it was for is no longer wanted.
/// Requests in flight finish; their results are still valid and simply cached.
#[derive(Clone)]
pub struct Cancel(Option<(Arc<AtomicU64>, u64)>);

impl Cancel {
    pub fn never() -> Self {
        Cancel(None)
    }

    pub fn tied_to(epoch: &Arc<AtomicU64>) -> Self {
        Cancel(Some((epoch.clone(), epoch.load(Ordering::Relaxed))))
    }

    pub fn is_stale(&self) -> bool {
        self.0
            .as_ref()
            .is_some_and(|(now, mine)| now.load(Ordering::Relaxed) != *mine)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_after_accepts_dates_and_rejects_invalid_values() {
        let now = std::time::UNIX_EPOCH + Duration::from_secs(784111777);
        assert_eq!(retry_after("Sun, 06 Nov 1994 08:49:37 GMT", now), Some(Duration::ZERO));
        assert_eq!(
            retry_after("Sun, 06 Nov 1994 08:49:57 GMT", now),
            Some(Duration::from_secs(20))
        );
        assert_eq!(
            retry_after("Sun, 06 Nov 1994 08:50:08 GMT", now),
            Some(Duration::from_secs(31))
        );
        assert_eq!(retry_after("20", now), Some(Duration::from_secs(20)));
        assert_eq!(retry_after("Sun, 31 Feb 1994 08:49:37 GMT", now), None);
        assert_eq!(retry_after("Sun, 06 Nov 1994 25:49:37 GMT", now), None);
        assert_eq!(
            next_step(429, retry_after("Sun, 06 Nov 1994 08:50:08 GMT", now), NEW),
            Step::Fail(ApiError::RateLimited)
        );
        assert_eq!(
            retry_after("Thu, 29 Feb 2024 00:00:00 GMT", std::time::UNIX_EPOCH),
            Some(Duration::from_secs(1709164800))
        );
        assert_eq!(retry_after("Thu, 29 Feb 2023 00:00:00 GMT", now), None);
        assert_eq!(retry_after("garbage", now), None);
    }

    const NEW: Seen = Seen {
        has_token: false,
        id_proven: false,
        may_refresh: true,
    };

    #[test]
    fn a_401_refreshes_once_then_blames_the_right_thing() {
        assert_eq!(next_step(401, None, NEW), Step::RefreshClientId);
        let spent = Seen {
            may_refresh: false,
            ..NEW
        };
        assert_eq!(next_step(401, None, spent), Step::Fail(ApiError::ClientId));
        let signed_in = Seen {
            has_token: true,
            ..spent
        };
        assert_eq!(next_step(401, None, signed_in), Step::Fail(ApiError::Auth));
    }

    #[test]
    fn a_403_is_a_refusal_once_the_client_id_has_worked() {
        assert_eq!(next_step(403, None, NEW), Step::RefreshClientId);
        let proven = Seen { id_proven: true, ..NEW };
        assert_eq!(next_step(403, None, proven), Step::Fail(ApiError::Forbidden));
    }

    #[test]
    fn rate_limits_follow_the_server_within_reason() {
        let wait = Duration::from_secs(7);
        assert_eq!(next_step(429, Some(wait), NEW), Step::Retry(Some(wait)));
        assert_eq!(next_step(429, None, NEW), Step::Retry(None));
        let long = MAX_RETRY_AFTER + Duration::from_secs(1);
        assert_eq!(next_step(429, Some(long), NEW), Step::Fail(ApiError::RateLimited));
    }

    #[test]
    fn server_errors_retry_and_client_errors_do_not() {
        assert_eq!(next_step(503, None, NEW), Step::Retry(None));
        assert_eq!(next_step(404, None, NEW), Step::Fail(ApiError::Unavailable));
        assert_eq!(next_step(400, None, NEW), Step::Fail(ApiError::Rejected(400)));
        assert_eq!(next_step(200, None, NEW), Step::Done);
    }

    #[test]
    fn backoff_grows_stays_bounded_and_varies() {
        for seed in 0..500 {
            let first = backoff(0, seed);
            assert!(first >= Duration::from_millis(200) && first <= Duration::from_millis(400));
            assert!(backoff(30, seed) <= Duration::from_secs(5));
            assert!(backoff(30, seed) >= Duration::from_millis(2500));
        }
        assert_ne!(backoff(2, 1), backoff(2, 2));
    }

    #[test]
    fn only_passing_problems_are_transient() {
        assert!(ApiError::Timeout.is_transient());
        assert!(ApiError::Server(503).is_transient());
        for e in [
            ApiError::Auth,
            ApiError::Forbidden,
            ApiError::Unavailable,
            ApiError::Protected,
        ] {
            assert!(!e.is_transient());
        }
    }

    #[test]
    fn the_gate_never_exceeds_its_limit() {
        let gate = Arc::new(Gate::new(2));
        let peak = Arc::new(Mutex::new((0usize, 0usize)));
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let (gate, peak) = (gate.clone(), peak.clone());
                std::thread::spawn(move || {
                    let _p = gate.enter(Instant::now() + REQUEST_DEADLINE, &Cancel::never()).unwrap();
                    {
                        let mut g = peak.lock().unwrap();
                        g.0 += 1;
                        g.1 = g.1.max(g.0);
                    }
                    std::thread::sleep(Duration::from_millis(5));
                    peak.lock().unwrap().0 -= 1;
                })
            })
            .collect();
        threads.into_iter().for_each(|t| t.join().unwrap());
        assert_eq!(peak.lock().unwrap().1, 2);
    }

    #[test]
    fn cancel_follows_its_epoch() {
        let epoch = Arc::new(AtomicU64::new(0));
        let c = Cancel::tied_to(&epoch);
        assert!(!c.is_stale());
        epoch.fetch_add(1, Ordering::Relaxed);
        assert!(c.is_stale());
        assert!(!Cancel::never().is_stale());
    }
}
