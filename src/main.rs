use std::{
    collections::HashMap,
    io::Write,
    net::{TcpListener, TcpStream},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

type RateLimiter = Arc<Mutex<HashMap<String, Vec<Instant>>>>;

fn main() {
    // starts the tcp server on port 8080
    // bind tries to grab port 8080 to use
    // expect is because if something goes wrong i want it to crash with a clear message
    let listener = TcpListener::bind("0.0.0.0:8080").expect("failed to bind");

    // creates a shared rate limiter using arc and mutex
    // arc lets multiple threads share the same data
    // mutex ensures only one thread can modify it at a time
    let limiter: RateLimiter = Arc::new(Mutex::new(HashMap::new()));

    // incoming waits for someone to connect
    // flatten removes the Option and Result that come along
    // this way we keep only the real connections
    for stream in listener.incoming().flatten() {
        // clones the arc reference for this thread
        // arc::clone only increments the reference count, doesn't copy the data
        let limiter = Arc::clone(&limiter);
        handle_connection(stream, limiter);
    }
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
        Err(_) => return, // if we can't get the ip, just drop the connection
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

    // creates a buffer to peek at the request
    // peek lets us see what arrived without consuming it
    let mut buf = [0; 4096];
    if stream.peek(&mut buf[..]).is_err() {
        return; // if we can't read, just give up
    }

    // converts the buffer bytes to string
    // from_utf8_lossy ignores invalid chars instead of panicking
    let req = String::from_utf8_lossy(&buf);

    // decides what to respond based on the request path
    // GET /ip returns json with ip info
    // GET / returns the html page
    // anything else gets a 404
    let (status, body, ctype) = if req.contains("GET /ip") {
        // extracts detailed ip information from headers and connection
        let ip_info = extract_ip_info(&req, &peer_ip);
        ("200 OK", ip_info, "application/json")
    } else if req.starts_with("GET / ") {
        // includes the html file at compile time
        // means it doesn't need external files to run
        (
            "200 OK",
            include_str!("index.html").to_string(),
            "text/html",
        )
    } else {
        ("404 Not Found", "not found".to_string(), "text/plain")
    };

    // sends the http response to the client
    send_response(&mut stream, status, ctype, &body);
}

fn extract_ip_info(req: &str, peer_ip: &str) -> String {
    // creates a hashmap to store http headers
    let mut headers = HashMap::new();

    // skips the first line (request line) and processes the rest
    // stops at the empty line that separates headers from body
    for line in req.lines().skip(1) {
        if line.is_empty() {
            break;
        }
        // splits each header line at the first colon
        // stores headers in lowercase for case-insensitive matching
        if let Some((key, value)) = line.split_once(':') {
            headers.insert(key.trim().to_lowercase(), value.trim().to_string());
        }
    }

    // tries to find the real public ip from various headers
    // checks them in order of preference
    let public_ip = headers
        .get("fly-client-ip")
        .or_else(|| headers.get("x-forwarded-for"))
        .or_else(|| headers.get("x-real-ip"))
        // x-forwarded-for can contain multiple ips, takes the first one
        .map(|s| s.split(',').next().unwrap_or(s).trim().to_string())
        // if no headers found, falls back to the direct peer ip
        .unwrap_or_else(|| peer_ip.to_string());

    // gets the user agent header or defaults to "unknown"
    let user_agent = headers
        .get("user-agent")
        .cloned()
        .unwrap_or_else(|| "unknown".to_string());

    // gets the full forwarded header or defaults to "none"
    let forwarded = headers
        .get("x-forwarded-for")
        .cloned()
        .unwrap_or_else(|| "none".to_string());

    // formats everything as a json string
    // escapes quotes in user agent to prevent breaking the json
    format!(
        r#"{{"public_ip":"{}","peer_ip":"{}","forwarded":"{}","user_agent":"{}"}}"#,
        public_ip,
        peer_ip,
        forwarded,
        user_agent.replace('"', "\\\"")
    )
}

fn check_rate_limit(limiter: &RateLimiter, ip: &str) -> bool {
    // tries to lock the mutex
    // if poisoned (another thread panicked), just allow the request
    let mut map = match limiter.lock() {
        Ok(m) => m,
        Err(_) => return true,
    };

    let now = Instant::now();
    let window = Duration::from_secs(60); // 1 minute window
    let max_requests = 30; // max 30 requests per minute per ip

    // gets or creates the timestamp vector for this ip
    let timestamps = map.entry(ip.to_string()).or_insert_with(Vec::new);

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
    // builds the http response string
    // includes status line, content type, content length, and body
    let response = format!(
        "HTTP/1.1 {}\r\nContent-Type: {}\r\nContent-Length: {}\r\n\r\n{}",
        status,
        ctype,
        body.len(),
        body
    );

    // writes the response to the stream
    // ok() ignores any write errors, not much we can do anyway
    stream.write_all(response.as_bytes()).ok();
    // flush ensures all data is sent immediately
    stream.flush().ok();
}
