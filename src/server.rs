use std::collections::HashMap;
use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;

const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Default)]
pub struct Request {
    pub method: String,
    pub path: String,
    pub query: HashMap<String, String>,
    pub headers: HashMap<String, String>,
    pub body: Vec<u8>,
}

pub enum Body {
    Bytes(Vec<u8>),
    File(PathBuf),
}

pub struct Response {
    pub status: u16,
    pub content_type: &'static str,
    pub body: Body,
}

impl Response {
    pub fn bytes(content_type: &'static str, bytes: Vec<u8>) -> Response {
        Response { status: 200, content_type, body: Body::Bytes(bytes) }
    }

    pub fn file(content_type: &'static str, path: PathBuf) -> Response {
        Response { status: 200, content_type, body: Body::File(path) }
    }

    pub fn error(status: u16, message: &str) -> Response {
        let body = serde_json::json!({ "error": message }).to_string().into_bytes();
        Response { status, content_type: "application/json", body: Body::Bytes(body) }
    }
}

/// Serves until the process ends, one thread per connection, each connection one request.
pub fn serve(listener: TcpListener, handler: Arc<dyn Fn(&Request) -> Response + Send + Sync>) -> io::Result<()> {
    let port = listener.local_addr()?.port();
    for stream in listener.incoming() {
        let (stream, handler) = (stream?, handler.clone());
        thread::spawn(move || {
            let _ = answer(stream, port, handler.as_ref());
        });
    }
    Ok(())
}

fn answer(stream: TcpStream, port: u16, handler: &(dyn Fn(&Request) -> Response + Send + Sync)) -> io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let request = match read_request(&mut reader)? {
        Some(request) => request,
        None => return write_response(stream, Response::error(400, "unreadable request"), None),
    };
    let response = if allowed(&request, port) { handler(&request) } else { Response::error(403, "this editor only answers its own page") };
    let range = request.headers.get("range").cloned();
    write_response(stream, response, range.as_deref())
}

/// The editor writes files and starts renders, so another site must not reach it from the
/// browser: not by pointing a name at this address (Host), nor by posting from its page (Origin).
fn allowed(request: &Request, port: u16) -> bool {
    let origins = [format!("127.0.0.1:{port}"), format!("localhost:{port}")];
    let host_ok = request.headers.get("host").is_some_and(|host| origins.contains(host));
    let origin_ok = request.headers.get("origin").is_none_or(|origin| origin.strip_prefix("http://").is_some_and(|o| origins.iter().any(|allowed| allowed == o)));
    host_ok && origin_ok
}

fn read_request(reader: &mut impl BufRead) -> io::Result<Option<Request>> {
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let mut parts = line.split_whitespace();
    let (Some(method), Some(target)) = (parts.next().map(String::from), parts.next().map(String::from)) else { return Ok(None) };
    let (path, query) = target.split_once('?').unwrap_or((&target, ""));
    let query = query.split('&').filter_map(|pair| pair.split_once('=')).map(|(k, v)| (k.to_string(), v.to_string())).collect();
    let mut headers = HashMap::new();
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 || line.trim().is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
        }
    }
    let length: usize = headers.get("content-length").and_then(|l: &String| l.parse().ok()).unwrap_or(0);
    if length > MAX_BODY_BYTES {
        return Ok(None);
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body)?;
    Ok(Some(Request { method, path: path.to_string(), query, headers, body }))
}

fn write_response(mut stream: TcpStream, response: Response, range: Option<&str>) -> io::Result<()> {
    let status_text = |status| match status {
        200 => "OK",
        206 => "Partial Content",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        _ => "Error",
    };
    let content_type = response.content_type;
    match response.body {
        Body::Bytes(bytes) => {
            write!(stream, "HTTP/1.1 {} {}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n", response.status, status_text(response.status), bytes.len())?;
            stream.write_all(&bytes)
        }
        Body::File(path) => {
            let mut file = File::open(&path)?;
            let total = file.metadata()?.len();
            let (status, start, end) = match range.and_then(|r| parse_range(r, total)) {
                Some((start, end)) => (206, start, end),
                None => (200, 0, total.saturating_sub(1)),
            };
            let length = if total == 0 { 0 } else { end - start + 1 };
            write!(stream, "HTTP/1.1 {status} {}\r\nContent-Type: {content_type}\r\nContent-Length: {length}\r\nAccept-Ranges: bytes\r\nCache-Control: no-store\r\nConnection: close\r\n", status_text(status))?;
            if status == 206 {
                write!(stream, "Content-Range: bytes {start}-{end}/{total}\r\n")?;
            }
            stream.write_all(b"\r\n")?;
            file.seek(SeekFrom::Start(start))?;
            io::copy(&mut file.take(length), &mut stream)?;
            Ok(())
        }
    }
}

/// The inclusive bytes a `Range` header asks for, if it asks for bytes this file has.
fn parse_range(header: &str, total: u64) -> Option<(u64, u64)> {
    let (first, last) = header.strip_prefix("bytes=")?.split_once('-')?;
    let last_byte = total.checked_sub(1)?;
    if first.is_empty() {
        let suffix: u64 = last.parse().ok()?;
        return (suffix > 0).then(|| (total.saturating_sub(suffix), last_byte));
    }
    let start: u64 = first.parse().ok()?;
    let end = if last.is_empty() { last_byte } else { last.parse::<u64>().ok()?.min(last_byte) };
    (start <= end).then_some((start, end))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    #[test]
    fn reads_a_range_of_bytes() {
        assert_eq!(parse_range("bytes=0-99", 1000), Some((0, 99)));
        assert_eq!(parse_range("bytes=900-", 1000), Some((900, 999)));
        assert_eq!(parse_range("bytes=900-5000", 1000), Some((900, 999)));
        assert_eq!(parse_range("bytes=-100", 1000), Some((900, 999)));
        assert_eq!(parse_range("bytes=-5000", 1000), Some((0, 999)));
        assert_eq!(parse_range("bytes=999-999", 1000), Some((999, 999)));
    }

    #[test]
    fn ignores_a_range_it_cannot_serve() {
        assert_eq!(parse_range("bytes=1000-", 1000), None);
        assert_eq!(parse_range("bytes=50-10", 1000), None);
        assert_eq!(parse_range("bytes=-0", 1000), None);
        assert_eq!(parse_range("items=0-1", 1000), None);
        assert_eq!(parse_range("bytes=0-1", 0), None);
    }

    fn request(host: &str, origin: Option<&str>) -> Request {
        let mut headers = HashMap::from([("host".to_string(), host.to_string())]);
        if let Some(origin) = origin {
            headers.insert("origin".to_string(), origin.to_string());
        }
        Request { headers, ..Request::default() }
    }

    #[test]
    fn answers_its_own_page_on_either_local_name() {
        assert!(allowed(&request("127.0.0.1:8000", None), 8000));
        assert!(allowed(&request("localhost:8000", Some("http://localhost:8000")), 8000));
        assert!(allowed(&request("127.0.0.1:8000", Some("http://127.0.0.1:8000")), 8000));
    }

    #[test]
    fn refuses_another_site() {
        assert!(!allowed(&request("evil.example:8000", None), 8000), "a rebound name");
        assert!(!allowed(&request("127.0.0.1:8000", Some("https://evil.example")), 8000), "a post from another page");
        assert!(!allowed(&request("127.0.0.1:8000", Some("http://127.0.0.1:9000")), 8000), "another local port");
        assert!(!allowed(&Request::default(), 8000), "no host at all");
    }

    fn exchange(port: u16, text: &str) -> String {
        let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
        stream.write_all(text.as_bytes()).unwrap();
        let mut reply = String::new();
        stream.read_to_string(&mut reply).unwrap();
        reply
    }

    #[test]
    fn serves_a_request_and_a_range_of_a_file_over_a_socket() {
        let dir = std::env::temp_dir().join(format!("reel-server-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("data.bin");
        std::fs::write(&file, "0123456789").unwrap();
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let handler: Arc<dyn Fn(&Request) -> Response + Send + Sync> = Arc::new(move |request| match request.path.as_str() {
            "/file" => Response::file("application/octet-stream", file.clone()),
            _ => Response::bytes("text/plain", [request.query["a"].as_bytes(), b":", &request.body].concat()),
        });
        thread::spawn(move || serve(listener, handler));

        let echo = exchange(port, &format!("POST /echo?a=b HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Length: 3\r\n\r\nxyz"));
        let part = exchange(port, &format!("GET /file HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nRange: bytes=2-4\r\n\r\n"));
        let whole = exchange(port, &format!("GET /file HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n"));
        let refused = exchange(port, "GET /file HTTP/1.1\r\nHost: evil.example\r\n\r\n");

        assert!(echo.starts_with("HTTP/1.1 200") && echo.ends_with("b:xyz"), "{echo}");
        assert!(part.starts_with("HTTP/1.1 206") && part.contains("Content-Range: bytes 2-4/10") && part.ends_with("\r\n\r\n234"), "{part}");
        assert!(whole.starts_with("HTTP/1.1 200") && whole.ends_with("0123456789"), "{whole}");
        assert!(refused.starts_with("HTTP/1.1 403"), "{refused}");
    }
}
