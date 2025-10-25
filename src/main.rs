use std::{io::Write, net::TcpListener};

fn main() {
    // here starts the tcp server on port 8080
    // bind tries to grab port 8080 to use
    // unwrap is because if something goes wrong i want it to crash anyway
    TcpListener::bind("0.0.0.0:8080")
        .unwrap()
        // incoming waits for someone to connect
        // this returns an iterator of streams
        .incoming()
        // flatten is used to remove the Option and Result that come along
        // this way we keep only the real connections
        .flatten()
        // for_each applies this function to each connection that arrives
        .for_each(|mut s| {
            // creates a 512 byte buffer to store the request
            // the empty array will receive the bytes from the client
            let mut buf = [0; 512];

            // peek takes a sneaky peek at what arrived without consuming it
            // ok() just ignores if it fails, nbd
            s.peek(&mut buf[..]).ok();

            // turns the buffer bytes into string
            // from_utf8_lossy just ignores invalid chars
            let req = String::from_utf8_lossy(&buf);

            // now it decides what to answer
            // if the request is GET /ip we send the client's IP
            // otherwise we send the html file
            let (body, ctype) = if req.contains("GET /ip") {
                // gets the IP of who connected and transforms it to string
                // unwrap again btw
                (s.peer_addr().unwrap().ip().to_string(), "text/plain")
            } else {
                // include_str! compiles the index.html file right into the executable
                // so we don't need external files to run, neat huh
                (include_str!("index.html").to_string(), "text/html")
            };

            // here it builds the HTTP response
            // write! returns a Result but we don't care about errors
            // ok() ignores everything and moves on, as usual
            write!(
                s,
                "HTTP/1.1 200 OK\r
Content-Type:{}\r
Content-Length:{}\r
\r
{}",
                ctype,      // content type, text/plain or text/html
                body.len(), // size of the response body in bytes
                body        // the actual content that goes to the client
            )
            .ok();

            // flush sends everything in the output buffer to the client at once
            s.flush().ok();
        });
}
