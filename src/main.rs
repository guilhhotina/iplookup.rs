use std::{
    collections::HashMap,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::{mpsc, Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

type RateLimiter = Arc<Mutex<HashMap<String, Vec<Instant>>>>;

struct IpInfo {
    public_ip: String,
    peer_ip: String,
    forwarded: String,
    user_agent: String,
}

// simple fixed size thread pool
struct ThreadPool {
    // sender side of the job channel
    tx: mpsc::Sender<Job>,
}

// boxed closure to run as a job
type Job = Box<dyn FnOnce() + Send + 'static>;

impl ThreadPool {
    // creates a new pool with N threads
    fn new(size: usize) -> Self {
        // creates channel for jobs
        let (tx, rx) = mpsc::channel::<Job>();
        // wrap receiver so many threads can block on it
        let rx = Arc::new(Mutex::new(rx));
        // spawn N threads that pull jobs and run them
        for _ in 0..size {
            // clone the shared receiver for this thread
            let rx = Arc::clone(&rx);
            // start the worker loop
            thread::spawn(move || {
                // keep receiving jobs until sender is dropped
                while let Ok(job) = rx.lock().unwrap().recv() {
                    // run the job
                    job();
                }
            });
        }
        // return the pool
        Self { tx }
    }

    // schedules a job to run on the pool
    fn execute<F>(&self, f: F)
    where
        F: FnOnce() + Send + 'static,
    {
        // send the job ignoring errors if pool is shutting down
        let _ = self.tx.send(Box::new(f));
    }
}

fn main() {
    // starts the tcp server on port 8080
    // bind tries to grab port 8080 to use
    // expect is because if something goes wrong i want it to crash with a clear message
    let listener = TcpListener::bind("0.0.0.0:8080").expect("failed to bind");

    // creates a shared rate limiter using arc and mutex
    // arc lets multiple threads share the same data
    // mutex ensures only one thread can modify it at a time
    let limiter: RateLimiter = Arc::new(Mutex::new(HashMap::new()));

    // creates a small thread pool sized to cpu * 4
    // unwrap_or uses 8 if detection fails
    let pool = ThreadPool::new(
        thread::available_parallelism()
            .map(|n| n.get() * 4)
            .unwrap_or(8),
    );

    // incoming waits for someone to connect
    // flatten removes the Option and Result that come along
    // this way we keep only the real connections
    for stream in listener.incoming().flatten() {
        // clones the arc reference for this task
        // arc::clone only increments the reference count, doesnt copy the data
        let limiter = Arc::clone(&limiter);
        // sends the work to the pool
        pool.execute(move || {
            // handles the connection inside a worker thread
            handle_connection(stream, limiter);
        });
    }
}

// actually reads the full request from the stream
// this is critical because peek doesnt consume data and can cause deadlocks
fn read_request(stream: &mut TcpStream) -> Result<String, std::io::Error> {
    let mut buf = [0; 8192]; // bigger buffer for safety
    let mut request = Vec::new();
    let mut content_length = 0;
    let mut body_start = 0;

    // read until we have complete headers
    loop {
        let n = stream.read(&mut buf)?;
        if n == 0 {
            break;
        }
        request.extend_from_slice(&buf[..n]);

        let req_str = String::from_utf8_lossy(&request);

        // look for end of headers
        if let Some(pos) = req_str.find("\r\n\r\n") {
            body_start = pos + 4;

            // extract content-length if present
            for line in req_str.lines() {
                if line.to_lowercase().starts_with("content-length:") {
                    if let Some(len_str) = line.split(':').nth(1) {
                        content_length = len_str.trim().parse().unwrap_or(0);
                    }
                    break;
                }
            }

            let body_received = request.len() - body_start;

            // if we got all the body or theres no body, were done
            if body_received >= content_length {
                break;
            }
        }

        // avoid infinite loop on malformed requests
        if request.len() > 16384 {
            break;
        }
    }

    Ok(String::from_utf8_lossy(&request).to_string())
}

fn handle_connection(mut stream: TcpStream, limiter: RateLimiter) {
    // sets read and write timeouts to 5 seconds
    // this prevents connections from hanging forever
    // ok() ignores if it fails, nbg
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
    stream.set_write_timeout(Some(Duration::from_secs(5))).ok();

    // gets the peer ip address
    // this is the direct ip of the connection (usually from proxy)
    let peer_ip = match stream.peer_addr() {
        Ok(addr) => addr.ip().to_string(),
        Err(_) => return, // if we cant get the ip, just drop the connection
    };

    // checks if this ip has exceeded the rate limit
    // if so, sends a 429 response and closes the connection
    if !check_rate_limit(&limiter, &peer_ip) {
        send_response(
            &mut stream,
            "429 Too Many Requests",
            "text/plain",
            "rate limit exceeded",
        );
        return;
    }

    // actually reads the full request now instead of just peeking
    // this is crucial to avoid the browser hanging on post requests
    let req = match read_request(&mut stream) {
        Ok(r) => r,
        Err(_) => return,
    };

    // grabs the first request line safely
    // this avoids false positives from searching the whole buffer
    let first_line = req.lines().next().unwrap_or("");

    // route logic for javascript-free experience
    let (status, body, ctype) = if first_line.starts_with("GET / ") {
        // serves initial page, no ip data
        let html_template = include_str!("index.html");
        let final_html = html_template.replace("{ip_info_placeholder}", "");
        ("200 OK", final_html, "text/html")
    } else if first_line.starts_with("POST / ") {
        // get data and updates page with ip
        let ip_info = extract_ip_info(&req, &peer_ip);
        let html_template = include_str!("index.html");
        let ip_info_html = format!(
            r#"
            <div class="info-line"><span class="label">public_ip:</span> <span class="value">{}</span></div>
            <div class="info-line"><span class="label">peer_ip:</span> <span class="value">{}</span></div>
            <div class="info-line"><span class="label">forwarded:</span> <span class="value">{}</span></div>
            <div class="info-line"><span class="label">user_agent:</span> <span class="value">{}</span></div>
            <div class="info-line"><span class="cursor">_</span></div>
            "#,
            ip_info.public_ip, ip_info.peer_ip, ip_info.forwarded, ip_info.user_agent
        );
        let final_html = html_template.replace("{ip_info_placeholder}", &ip_info_html);
        ("200 OK", final_html, "text/html")
    } else {
        ("404 Not Found", "not found".to_string(), "text/plain")
    };

    // sends the http response to the client
    send_response(&mut stream, status, ctype, &body);
}

fn extract_ip_info(req: &str, peer_ip: &str) -> IpInfo {
    // picks only the headers we care about while scanning
    let mut fly = None; // fly-client-ip value if present
    let mut xff = None; // x-forwarded-for full list if present
    let mut xreal = None; // x-real-ip value if present
    let mut ua = None; // user-agent value if present

    // skips the request line and reads headers until the empty line
    for line in req.lines().skip(1) {
        // blank line ends headers
        if line.is_empty() {
            break;
        }
        // splits at the first colon
        if let Some((k, v)) = line.split_once(':') {
            // trim spaces around key and value
            let key = k.trim();
            let val = v.trim();
            // match header names case-insensitively
            if key.eq_ignore_ascii_case("fly-client-ip") {
                fly = Some(val.to_string());
            } else if key.eq_ignore_ascii_case("x-forwarded-for") {
                xff = Some(val.to_string());
            } else if key.eq_ignore_ascii_case("x-real-ip") {
                xreal = Some(val.to_string());
            } else if key.eq_ignore_ascii_case("user-agent") {
                ua = Some(val.to_string());
            }
        }
    }

    // chooses public ip preferring fly then first from x-forwarded-for then x-real-ip then peer
    let public_ip = fly
        .or_else(|| {
            xff.as_ref()
                .map(|s| s.split(',').next().unwrap_or("").trim().to_string())
        })
        .or_else(|| xreal.clone())
        .unwrap_or_else(|| peer_ip.to_string());

    // takes the full forwarded chain or none
    let forwarded = xff.unwrap_or_else(|| "none".to_string());

    // takes user agent or unknown
    let user_agent = ua.unwrap_or_else(|| "unknown".to_string());

    IpInfo {
        public_ip,
        peer_ip: peer_ip.to_string(),
        forwarded,
        user_agent,
    }
}

fn check_rate_limit(limiter: &RateLimiter, ip: &str) -> bool {
    // tries to lock the mutex
    // if poisoned just allow the request
    let mut map = match limiter.lock() {
        Ok(m) => m,
        Err(_) => return true,
    };

    // captures current time
    let now = Instant::now();
    // 1 minute window
    let window = Duration::from_secs(60);
    // max 30 requests per minute per ip
    let max_requests = 30;

    // gets or creates the timestamp vector for this ip
    let timestamps = map.entry(ip.to_owned()).or_default();

    // removes old timestamps outside the window
    timestamps.retain(|&t| now.duration_since(t) < window);

    // if already at the limit, deny the request
    if timestamps.len() >= max_requests {
        return false;
    }

    // adds current timestamp and allow the request
    timestamps.push(now);
    true
}

fn send_response(stream: &mut TcpStream, status: &str, ctype: &str, body: &str) {
    // writes the http response directly without building a big string
    let _ = write!(
        stream,
        "HTTP/1.1 {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        status,
        ctype,
        body.len(),
        body
    );
    // flush ensures all data is sent immediately
    let _ = stream.flush();
}
