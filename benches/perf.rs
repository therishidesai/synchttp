use std::env;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::{Duration, Instant};

use synchttp::{Response, Router, Server, ServerConfig, StatusCode};

#[derive(Clone)]
enum Scenario {
    Hello,
    Echo { payload: String },
}

impl Scenario {
    fn name(&self) -> &'static str {
        match self {
            Scenario::Hello => "hello",
            Scenario::Echo { .. } => "echo",
        }
    }
}

struct BenchConfig {
    warmup: Duration,
    throughput_duration: Duration,
    throughput_threads: usize,
    latency_threads: usize,
    latency_samples: usize,
    latency_warmup_requests: usize,
    echo_body_bytes: usize,
}

impl BenchConfig {
    fn from_env() -> Self {
        let cpu_count = thread::available_parallelism()
            .map(|count| count.get())
            .unwrap_or(1);

        Self {
            warmup: Duration::from_secs(env_u64("SYNCHTTP_BENCH_WARMUP_SECS", 1)),
            throughput_duration: Duration::from_secs(env_u64("SYNCHTTP_BENCH_DURATION_SECS", 2)),
            throughput_threads: env_usize("SYNCHTTP_BENCH_THREADS", cpu_count.min(8).max(1)),
            latency_threads: env_usize("SYNCHTTP_BENCH_LATENCY_THREADS", 1),
            latency_samples: env_usize("SYNCHTTP_BENCH_LATENCY_SAMPLES", 1000),
            latency_warmup_requests: env_usize("SYNCHTTP_BENCH_LATENCY_WARMUP", 100),
            echo_body_bytes: env_usize("SYNCHTTP_BENCH_ECHO_BYTES", 256),
        }
    }
}

struct BenchServer {
    base_url: String,
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl BenchServer {
    fn base_url(&self) -> &str {
        &self.base_url
    }
}

impl Drop for BenchServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            handle.join().expect("benchmark server thread should join");
        }
    }
}

struct ThroughputResult {
    requests: usize,
    elapsed: Duration,
}

struct LatencyResult {
    samples: usize,
    mean_us: f64,
    p50_us: u64,
    p90_us: u64,
    p99_us: u64,
    max_us: u64,
}

fn main() {
    let config = BenchConfig::from_env();
    let hello = Scenario::Hello;
    let echo = Scenario::Echo {
        payload: build_json_payload(config.echo_body_bytes),
    };

    println!("synchttp performance benchmark");
    println!(
        "warmup={}s throughput_duration={}s throughput_threads={} latency_threads={} latency_samples={} echo_body_bytes={}",
        config.warmup.as_secs(),
        config.throughput_duration.as_secs(),
        config.throughput_threads,
        config.latency_threads,
        config.latency_samples,
        config.echo_body_bytes,
    );
    println!();

    run_throughput_benchmark(&config, &hello);
    run_throughput_benchmark(&config, &echo);
    run_latency_benchmark(&config, &hello);
    run_latency_benchmark(&config, &echo);
}

fn run_throughput_benchmark(config: &BenchConfig, scenario: &Scenario) {
    let server = spawn_server();

    let _ = run_worker_phase(
        config.throughput_threads,
        config.warmup,
        server.base_url().to_string(),
        scenario.clone(),
    );
    let result = run_worker_phase(
        config.throughput_threads,
        config.throughput_duration,
        server.base_url().to_string(),
        scenario.clone(),
    );

    let requests_per_sec = result.requests as f64 / result.elapsed.as_secs_f64();
    println!(
        "throughput {:>5}: threads={:<2} requests={:<8} elapsed={:>6.2}s req/s={:>10.2}",
        scenario.name(),
        config.throughput_threads,
        result.requests,
        result.elapsed.as_secs_f64(),
        requests_per_sec,
    );
}

fn run_latency_benchmark(config: &BenchConfig, scenario: &Scenario) {
    let server = spawn_server();
    let result = run_latency_phase(
        config.latency_threads,
        config.latency_samples,
        config.latency_warmup_requests,
        server.base_url().to_string(),
        scenario.clone(),
    );

    println!(
        "latency    {:>5}: threads={:<2} samples={:<6} mean={:>9.2}us p50={:>8}us p90={:>8}us p99={:>8}us max={:>8}us",
        scenario.name(),
        config.latency_threads,
        result.samples,
        result.mean_us,
        result.p50_us,
        result.p90_us,
        result.p99_us,
        result.max_us,
    );
}

fn spawn_server() -> BenchServer {
    let stop = Arc::new(AtomicBool::new(false));
    let stop_for_thread = Arc::clone(&stop);
    let router = Router::new()
        .get("/hello", |_req| Response::text(StatusCode::OK, "hello"))
        .post("/echo", |req| {
            Response::bytes(StatusCode::OK, req.body().to_vec())
                .header("content-type", "application/json")
        });

    let server = Server::bind("127.0.0.1:0")
        .expect("benchmark server should bind")
        .with_config(ServerConfig::default().poll_timeout(Duration::from_millis(5)));
    let addr = server
        .local_addr()
        .expect("benchmark server should have local addr");
    let handle = thread::spawn(move || {
        server
            .serve_until(router, || stop_for_thread.load(Ordering::Relaxed))
            .expect("benchmark server should run");
    });

    BenchServer {
        base_url: format!("http://{}", addr),
        stop,
        handle: Some(handle),
    }
}

fn run_worker_phase(
    threads: usize,
    duration: Duration,
    base_url: String,
    scenario: Scenario,
) -> ThroughputResult {
    let stop = Arc::new(AtomicBool::new(false));
    let barrier = Arc::new(Barrier::new(threads + 1));
    let mut handles = Vec::with_capacity(threads);

    for _ in 0..threads {
        let stop = Arc::clone(&stop);
        let barrier = Arc::clone(&barrier);
        let base_url = base_url.clone();
        let scenario = scenario.clone();
        handles.push(thread::spawn(move || {
            let agent = build_agent();
            let mut completed = 0usize;

            barrier.wait();
            while !stop.load(Ordering::Relaxed) {
                perform_request(&agent, &base_url, &scenario)
                    .expect("throughput benchmark request should succeed");
                completed += 1;
            }

            completed
        }));
    }

    barrier.wait();
    let start = Instant::now();
    thread::sleep(duration);
    stop.store(true, Ordering::Relaxed);

    let mut requests = 0usize;
    for handle in handles {
        requests += handle.join().expect("throughput worker should join");
    }

    ThroughputResult {
        requests,
        elapsed: start.elapsed(),
    }
}

fn run_latency_phase(
    threads: usize,
    total_samples: usize,
    warmup_requests: usize,
    base_url: String,
    scenario: Scenario,
) -> LatencyResult {
    let barrier = Arc::new(Barrier::new(threads));
    let samples_per_thread = (total_samples + threads - 1) / threads;
    let mut handles = Vec::with_capacity(threads);

    for _ in 0..threads {
        let barrier = Arc::clone(&barrier);
        let base_url = base_url.clone();
        let scenario = scenario.clone();
        handles.push(thread::spawn(move || {
            let agent = build_agent();
            let mut samples = Vec::with_capacity(samples_per_thread);

            for _ in 0..warmup_requests {
                perform_request(&agent, &base_url, &scenario)
                    .expect("latency warmup request should succeed");
            }

            barrier.wait();
            for _ in 0..samples_per_thread {
                let start = Instant::now();
                perform_request(&agent, &base_url, &scenario)
                    .expect("latency benchmark request should succeed");
                samples.push(start.elapsed().as_micros() as u64);
            }

            samples
        }));
    }

    let mut all_samples = Vec::with_capacity(samples_per_thread * threads);
    for handle in handles {
        all_samples.extend(handle.join().expect("latency worker should join"));
    }
    all_samples.truncate(total_samples);
    all_samples.sort_unstable();

    let samples = all_samples.len();
    let total: u128 = all_samples.iter().map(|value| *value as u128).sum();

    LatencyResult {
        samples,
        mean_us: total as f64 / samples as f64,
        p50_us: percentile(&all_samples, 0.50),
        p90_us: percentile(&all_samples, 0.90),
        p99_us: percentile(&all_samples, 0.99),
        max_us: *all_samples.last().unwrap_or(&0),
    }
}

fn build_agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(5))
        .timeout_read(Duration::from_secs(5))
        .timeout_write(Duration::from_secs(5))
        .build()
}

fn perform_request(agent: &ureq::Agent, base_url: &str, scenario: &Scenario) -> Result<(), String> {
    match scenario {
        Scenario::Hello => {
            let response = agent
                .get(&format!("{}/hello", base_url))
                .call()
                .map_err(|error| error.to_string())?;

            if response.status() != 200 {
                return Err(format!("unexpected hello status {}", response.status()));
            }

            let body = response.into_string().map_err(|error| error.to_string())?;
            if body != "hello" {
                return Err(format!("unexpected hello body {:?}", body));
            }
        }
        Scenario::Echo { payload } => {
            let response = agent
                .post(&format!("{}/echo", base_url))
                .set("content-type", "application/json")
                .send_string(payload)
                .map_err(|error| error.to_string())?;

            if response.status() != 200 {
                return Err(format!("unexpected echo status {}", response.status()));
            }

            let body = response.into_string().map_err(|error| error.to_string())?;
            if body != *payload {
                return Err(format!(
                    "unexpected echo body length {} expected {}",
                    body.len(),
                    payload.len()
                ));
            }
        }
    }

    Ok(())
}

fn build_json_payload(target_bytes: usize) -> String {
    let prefix = "{\"message\":\"";
    let suffix = "\"}";
    let minimum_len = prefix.len() + suffix.len() + 1;
    let fill_len = target_bytes
        .saturating_sub(prefix.len() + suffix.len())
        .max(1);

    let mut payload = String::with_capacity(target_bytes.max(minimum_len));
    payload.push_str(prefix);
    payload.push_str(&"x".repeat(fill_len));
    payload.push_str(suffix);
    payload
}

fn percentile(samples: &[u64], quantile: f64) -> u64 {
    if samples.is_empty() {
        return 0;
    }

    let index = ((samples.len() - 1) as f64 * quantile).round() as usize;
    samples[index.min(samples.len() - 1)]
}

fn env_usize(name: &str, default: usize) -> usize {
    env::var(name)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

fn env_u64(name: &str, default: u64) -> u64 {
    env::var(name)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}
