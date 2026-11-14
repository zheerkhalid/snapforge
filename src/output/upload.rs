//! Upload via FTP (small built-in client) or SFTP (Windows OpenSSH `sftp.exe`, DECISIONS #5).

use crate::settings::{secret, UploadProtocol, UploadSettings};
use anyhow::{anyhow, bail, ensure, Context, Result};
use std::io::{BufRead, BufReader, Write};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const TIMEOUT: Duration = Duration::from_secs(30);
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Upload `path` via FTP or SFTP. Returns the public URL when `url_prefix` is set.
pub fn upload_file(path: &Path, settings: &UploadSettings) -> Result<Option<String>> {
    ensure!(!settings.host.trim().is_empty(), "no upload server is configured");
    ensure!(path.is_file(), "cannot upload {}: the file does not exist", path.display());
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .ok_or_else(|| anyhow!("invalid file name: {}", path.display()))?;
    match settings.protocol {
        UploadProtocol::Ftp => {
            let password = secret::unprotect(&settings.password_protected)
                .ok_or_else(|| anyhow!("the stored FTP password cannot be decrypted; enter it again in Settings"))?;
            let data = std::fs::read(path).with_context(|| format!("could not read {}", path.display()))?;
            ftp_upload(settings, &password, &name, &data)?;
        }
        UploadProtocol::Sftp => {
            if !settings.password_protected.is_empty() {
                log::warn!("upload: SFTP uses key-based authentication only; the stored password is ignored");
            }
            sftp_upload(settings, path, &name)?;
        }
    }
    log::info!("upload: {} → {}{}", path.display(), settings.host, settings.remote_dir);
    Ok(public_url(&settings.url_prefix, &name))
}

/// `prefix` + percent-encoded file name, or None if no prefix is configured.
pub fn public_url(prefix: &str, name: &str) -> Option<String> {
    let prefix = prefix.trim();
    if prefix.is_empty() {
        return None;
    }
    let sep = if prefix.ends_with('/') { "" } else { "/" };
    Some(format!("{prefix}{sep}{}", percent_encode(name)))
}

pub fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Remote directory components ("/a/b/" → ["a", "b"]) and whether it is absolute.
fn dir_parts(dir: &str) -> (bool, Vec<&str>) {
    let d = dir.trim();
    (d.starts_with('/'), d.split('/').filter(|s| !s.is_empty() && *s != ".").collect())
}

// ---------------------------------------------------------------- FTP

struct Ftp {
    ctrl: BufReader<TcpStream>,
    peer: IpAddr,
}

struct Reply {
    code: u16,
    text: String,
}

impl Reply {
    fn ok(&self) -> bool {
        (200..400).contains(&self.code)
    }
}

fn connect_tcp(host: &str, port: u16) -> Result<TcpStream> {
    let addrs: Vec<SocketAddr> =
        (host, port).to_socket_addrs().with_context(|| format!("could not resolve {host}"))?.collect();
    let mut last = None;
    for a in &addrs {
        match TcpStream::connect_timeout(a, TIMEOUT) {
            Ok(s) => {
                s.set_read_timeout(Some(TIMEOUT))?;
                s.set_write_timeout(Some(TIMEOUT))?;
                return Ok(s);
            }
            Err(e) => last = Some(e),
        }
    }
    match last {
        Some(e) => Err(anyhow!("could not connect to {host}:{port}: {e}")),
        None => bail!("{host} has no addresses"),
    }
}

impl Ftp {
    fn connect(host: &str, port: u16) -> Result<Self> {
        let s = connect_tcp(host, port)?;
        let peer = s.peer_addr()?.ip();
        let mut ftp = Ftp { ctrl: BufReader::new(s), peer };
        let greet = ftp.read_reply()?;
        ensure!(greet.code == 220, "FTP server refused the connection: {} {}", greet.code, greet.text);
        Ok(ftp)
    }

    fn read_line(&mut self) -> Result<String> {
        let mut line = String::new();
        let n = self.ctrl.read_line(&mut line).context("FTP connection lost")?;
        ensure!(n > 0, "FTP server closed the connection");
        Ok(line.trim_end_matches(['\r', '\n']).to_string())
    }

    /// Read a (possibly multi-line, "123-…" … "123 …") reply.
    fn read_reply(&mut self) -> Result<Reply> {
        let first = self.read_line()?;
        let code: u16 =
            first.get(..3).and_then(|c| c.parse().ok()).ok_or_else(|| anyhow!("malformed FTP reply: {first:?}"))?;
        let mut text = first.get(4..).unwrap_or("").to_string();
        if first.as_bytes().get(3) == Some(&b'-') {
            let end = format!("{code} ");
            loop {
                let l = self.read_line()?;
                if l.starts_with(&end) || l == code.to_string() {
                    text.push('\n');
                    text.push_str(l.get(4..).unwrap_or(""));
                    break;
                }
                text.push('\n');
                text.push_str(&l);
            }
        }
        log::trace!("ftp < {code} {text}");
        Ok(Reply { code, text })
    }

    fn cmd(&mut self, c: &str) -> Result<Reply> {
        if c.starts_with("PASS ") {
            log::trace!("ftp > PASS ****");
        } else {
            log::trace!("ftp > {c}");
        }
        let s = self.ctrl.get_mut();
        s.write_all(c.as_bytes())?;
        s.write_all(b"\r\n")?;
        s.flush()?;
        self.read_reply()
    }

    fn expect(&mut self, c: &str, what: &str) -> Result<Reply> {
        let r = self.cmd(c)?;
        ensure!(r.ok(), "FTP {what} failed: {} {}", r.code, r.text);
        Ok(r)
    }

    fn login(&mut self, user: &str, pass: &str) -> Result<()> {
        let user = if user.is_empty() { "anonymous" } else { user };
        let r = self.cmd(&format!("USER {user}"))?;
        match r.code {
            230 => return Ok(()),
            331 | 332 => {}
            _ => bail!("FTP login rejected: {} {}", r.code, r.text),
        }
        let r = self.cmd(&format!("PASS {pass}"))?;
        ensure!(
            r.code == 230 || r.code == 202,
            "FTP login failed (check user name and password): {} {}",
            r.code,
            r.text
        );
        Ok(())
    }

    /// Change into `dir`, creating missing directories.
    fn cwd_create(&mut self, dir: &str) -> Result<()> {
        let (abs, parts) = dir_parts(dir);
        if abs {
            self.expect("CWD /", "change to /")?;
        }
        for p in parts {
            if self.cmd(&format!("CWD {p}"))?.ok() {
                continue;
            }
            let mk = self.cmd(&format!("MKD {p}"))?;
            if !mk.ok() {
                log::debug!("ftp: MKD {p} → {} {}", mk.code, mk.text);
            }
            let r = self.cmd(&format!("CWD {p}"))?;
            ensure!(r.ok(), "FTP: could not create or open remote folder \"{p}\": {} {}", r.code, r.text);
        }
        Ok(())
    }

    fn passive_data(&mut self) -> Result<TcpStream> {
        let r = self.cmd("EPSV")?;
        let addr = if r.code == 229 {
            let port = parse_epsv(&r.text).ok_or_else(|| anyhow!("malformed EPSV reply: {}", r.text))?;
            SocketAddr::new(self.peer, port)
        } else {
            let r = self.expect("PASV", "passive mode")?;
            let (_ip, port) = parse_pasv(&r.text).ok_or_else(|| anyhow!("malformed PASV reply: {}", r.text))?;
            // Use the control connection's address: servers behind NAT often report a private IP.
            SocketAddr::new(self.peer, port)
        };
        let s = TcpStream::connect_timeout(&addr, TIMEOUT).with_context(|| format!("FTP data connection to {addr}"))?;
        s.set_write_timeout(Some(TIMEOUT))?;
        Ok(s)
    }

    fn active_listener(&mut self) -> Result<TcpListener> {
        let local = self.ctrl.get_ref().local_addr()?.ip();
        let l = TcpListener::bind(SocketAddr::new(local, 0))?;
        let port = l.local_addr()?.port();
        let ok = match local {
            IpAddr::V4(v4) => {
                let o = v4.octets();
                self.cmd(&format!("PORT {},{},{},{},{},{}", o[0], o[1], o[2], o[3], port >> 8, port & 0xFF))?.ok()
            }
            IpAddr::V6(v6) => self.cmd(&format!("EPRT |2|{v6}|{port}|"))?.ok(),
        };
        ensure!(ok, "FTP server rejected active mode (try passive mode)");
        Ok(l)
    }

    fn stor(&mut self, name: &str, data: &[u8], passive: bool) -> Result<()> {
        let mut stream = if passive {
            let s = self.passive_data()?;
            self.start_transfer(name)?;
            s
        } else {
            let l = self.active_listener()?;
            self.start_transfer(name)?;
            accept_timeout(&l, TIMEOUT)?
        };
        stream.write_all(data).context("FTP upload interrupted")?;
        stream.flush()?;
        let _ = stream.shutdown(std::net::Shutdown::Both);
        drop(stream);
        let r = self.read_reply()?;
        ensure!(r.code == 226 || r.code == 250, "FTP upload was not completed: {} {}", r.code, r.text);
        Ok(())
    }

    fn start_transfer(&mut self, name: &str) -> Result<()> {
        let r = self.cmd(&format!("STOR {name}"))?;
        ensure!(r.code == 125 || r.code == 150, "FTP server refused the upload: {} {}", r.code, r.text);
        Ok(())
    }
}

fn accept_timeout(l: &TcpListener, timeout: Duration) -> Result<TcpStream> {
    l.set_nonblocking(true)?;
    let start = Instant::now();
    loop {
        match l.accept() {
            Ok((s, _)) => {
                s.set_nonblocking(false)?;
                s.set_write_timeout(Some(TIMEOUT))?;
                return Ok(s);
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                ensure!(start.elapsed() < timeout, "FTP server did not open the data connection (active mode)");
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => return Err(e.into()),
        }
    }
}

/// "Entering Extended Passive Mode (|||6446|)" → 6446.
pub fn parse_epsv(text: &str) -> Option<u16> {
    let start = text.find('(')?;
    let end = text[start..].find(')')? + start;
    let inner = &text[start + 1..end];
    let delim = inner.chars().next()?;
    inner.split(delim).rfind(|s| !s.is_empty())?.parse().ok()
}

/// "Entering Passive Mode (h1,h2,h3,h4,p1,p2)" → (ip, port).
pub fn parse_pasv(text: &str) -> Option<([u8; 4], u16)> {
    let start = text.find(|c: char| c.is_ascii_digit())?;
    let nums: Vec<u8> = text[start..]
        .split(|c: char| !c.is_ascii_digit())
        .filter(|s| !s.is_empty())
        .take(6)
        .map(|s| s.parse().ok())
        .collect::<Option<_>>()?;
    if nums.len() != 6 {
        return None;
    }
    Some(([nums[0], nums[1], nums[2], nums[3]], (nums[4] as u16) << 8 | nums[5] as u16))
}

fn ftp_upload(s: &UploadSettings, password: &str, name: &str, data: &[u8]) -> Result<()> {
    let port = if s.port == 0 { 21 } else { s.port };
    let mut ftp = Ftp::connect(s.host.trim(), port)?;
    ftp.login(&s.username, password)?;
    ftp.expect("TYPE I", "binary mode")?;
    ftp.cwd_create(&s.remote_dir)?;
    ftp.stor(name, data, s.passive)?;
    let _ = ftp.cmd("QUIT");
    Ok(())
}

// ---------------------------------------------------------------- SFTP

fn find_sftp() -> Option<PathBuf> {
    let sysroot = std::env::var_os("SystemRoot").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
    let builtin = sysroot.join(r"System32\OpenSSH\sftp.exe");
    if builtin.is_file() {
        return Some(builtin);
    }
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).map(|d| d.join("sftp.exe")).find(|c| c.is_file()))
        .unwrap_or(None)
}

fn sftp_quote(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "/").replace('"', "\\\""))
}

/// Batch script: create the remote folders (ignoring "exists" errors), cd, put.
pub fn sftp_script(remote_dir: &str, local: &Path, name: &str) -> String {
    let (abs, parts) = dir_parts(remote_dir);
    let mut script = String::new();
    let mut cur = if abs { String::from("/") } else { String::new() };
    for p in &parts {
        cur.push_str(p);
        script.push_str(&format!("-mkdir {}\n", sftp_quote(&cur)));
        cur.push('/');
    }
    if !cur.is_empty() {
        script.push_str(&format!("cd {}\n", sftp_quote(&cur)));
    }
    script.push_str(&format!("put {} {}\n", sftp_quote(&local.to_string_lossy()), sftp_quote(name)));
    script.push_str("bye\n");
    script
}

pub fn sftp_args(s: &UploadSettings, script: &Path) -> Vec<String> {
    let port = if s.port == 0 { 22 } else { s.port };
    let mut a = vec![
        "-b".to_string(),
        script.to_string_lossy().into_owned(),
        "-oBatchMode=yes".into(),
        "-oStrictHostKeyChecking=accept-new".into(),
        "-oConnectTimeout=20".into(),
        "-P".into(),
        port.to_string(),
    ];
    if !s.sftp_key_path.trim().is_empty() {
        a.push("-i".into());
        a.push(s.sftp_key_path.trim().to_string());
    }
    let host = s.host.trim();
    let host = if host.contains(':') && !host.starts_with('[') { format!("[{host}]") } else { host.to_string() };
    a.push(if s.username.trim().is_empty() { host } else { format!("{}@{host}", s.username.trim()) });
    a
}

/// Deletes the script file on drop.
struct TempFile(PathBuf);
impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn sftp_upload(s: &UploadSettings, local: &Path, name: &str) -> Result<()> {
    let exe = find_sftp().ok_or_else(|| {
        anyhow!(
            "sftp.exe was not found. Install the Windows \"OpenSSH Client\" optional feature \
             (Settings > System > Optional features) to upload with SFTP."
        )
    })?;
    let local = std::fs::canonicalize(local).unwrap_or_else(|_| local.to_path_buf());
    let local_s = local.to_string_lossy();
    let local = PathBuf::from(local_s.strip_prefix(r"\\?\").unwrap_or(&local_s));
    let script = TempFile(std::env::temp_dir().join(format!(
        "snapforge-sftp-{}-{}.txt",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0)
    )));
    std::fs::write(&script.0, sftp_script(&s.remote_dir, &local, name))
        .with_context(|| format!("could not write {}", script.0.display()))?;
    let args = sftp_args(s, &script.0);
    log::debug!("upload: {} {}", exe.display(), args.join(" "));
    let out = std::process::Command::new(&exe)
        .args(&args)
        .stdin(std::process::Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .with_context(|| format!("could not run {}", exe.display()))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let err = err.trim();
        let hint = if err.contains("Permission denied") {
            " (SFTP needs key-based login: set a private key file and add its public key to the server)"
        } else {
            ""
        };
        bail!("SFTP upload failed{hint}: {}", if err.is_empty() { "unknown error" } else { err });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::sync::{Arc, Mutex};

    /// Scripted fake FTP server. Records commands and the uploaded bytes.
    struct Fake {
        port: u16,
        log: Arc<Mutex<Vec<String>>>,
        stored: Stored,
        handle: std::thread::JoinHandle<()>,
    }

    type Stored = Arc<Mutex<Option<(String, Vec<u8>)>>>;

    fn here(cwd: &[String]) -> String {
        cwd.iter().map(|c| format!("/{c}")).collect()
    }

    fn fake_server(existing_dirs: &'static [&'static str], support_epsv: bool) -> Fake {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let log = Arc::new(Mutex::new(Vec::new()));
        let stored = Arc::new(Mutex::new(None));
        let (log2, stored2) = (log.clone(), stored.clone());
        let handle = std::thread::spawn(move || {
            let (s, _) = listener.accept().unwrap();
            s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
            let mut r = BufReader::new(s.try_clone().unwrap());
            let mut w = s;
            let send = |w: &mut TcpStream, t: &str| w.write_all(format!("{t}\r\n").as_bytes()).unwrap();
            send(&mut w, "220-Welcome to the fake server\r\n220-second line\r\n220 ready");
            let mut cwd: Vec<String> = Vec::new();
            let mut dirs: Vec<String> = existing_dirs.iter().map(|s| s.to_string()).collect();
            let mut data_listener: Option<TcpListener> = None;
            loop {
                let mut line = String::new();
                if r.read_line(&mut line).unwrap_or(0) == 0 {
                    break;
                }
                let line = line.trim_end().to_string();
                log2.lock().unwrap().push(line.clone());
                let (cmd, arg) = line.split_once(' ').unwrap_or((&line, ""));
                match cmd {
                    "USER" => send(&mut w, "331 password please"),
                    "PASS" if arg == "pa ss" => send(&mut w, "230 logged in"),
                    "PASS" => send(&mut w, "530 bad password"),
                    "TYPE" => send(&mut w, "200 binary"),
                    "CWD" if arg == "/" => {
                        cwd.clear();
                        send(&mut w, "250 ok")
                    }
                    "CWD" => {
                        let p = format!("{}/{arg}", here(&cwd));
                        if dirs.contains(&p) {
                            cwd.push(arg.to_string());
                            send(&mut w, "250 ok")
                        } else {
                            send(&mut w, "550 no such directory")
                        }
                    }
                    "MKD" => {
                        dirs.push(format!("{}/{arg}", here(&cwd)));
                        send(&mut w, "257 created")
                    }
                    "EPSV" if support_epsv => {
                        let l = TcpListener::bind("127.0.0.1:0").unwrap();
                        let p = l.local_addr().unwrap().port();
                        data_listener = Some(l);
                        send(&mut w, &format!("229 Entering Extended Passive Mode (|||{p}|)"))
                    }
                    "EPSV" => send(&mut w, "500 unknown command"),
                    "PASV" => {
                        let l = TcpListener::bind("127.0.0.1:0").unwrap();
                        let p = l.local_addr().unwrap().port();
                        data_listener = Some(l);
                        // Report a bogus private IP to check that the client uses the peer address.
                        send(&mut w, &format!("227 Entering Passive Mode (10,9,8,7,{},{})", p >> 8, p & 255))
                    }
                    "STOR" => {
                        send(&mut w, "150 opening data connection");
                        let l = data_listener.take().unwrap();
                        let (mut d, _) = l.accept().unwrap();
                        let mut buf = Vec::new();
                        d.read_to_end(&mut buf).unwrap();
                        *stored2.lock().unwrap() = Some((format!("{}/{arg}", here(&cwd)), buf));
                        send(&mut w, "226-transfer complete\r\n226 bye")
                    }
                    "QUIT" => {
                        send(&mut w, "221 goodbye");
                        break;
                    }
                    _ => send(&mut w, "502 not implemented"),
                }
            }
        });
        Fake { port, log, stored, handle }
    }

    fn settings(port: u16, dir: &str) -> UploadSettings {
        UploadSettings {
            host: "127.0.0.1".into(),
            port,
            username: "bob".into(),
            remote_dir: dir.into(),
            ..Default::default()
        }
    }

    #[test]
    fn ftp_upload_epsv_creates_dirs() {
        let fake = fake_server(&["/pub"], true);
        let data: Vec<u8> = (0..100_000u32).map(|i| (i * 7) as u8).collect();
        ftp_upload(&settings(fake.port, "/pub/shots/2026/"), "pa ss", "a b.png", &data).unwrap();
        fake.handle.join().unwrap();
        let (path, bytes) = fake.stored.lock().unwrap().take().unwrap();
        assert_eq!(path, "/pub/shots/2026/a b.png");
        assert_eq!(bytes, data);
        let log = fake.log.lock().unwrap();
        assert!(log.contains(&"MKD shots".to_string()));
        assert!(log.contains(&"MKD 2026".to_string()));
        assert!(!log.contains(&"MKD pub".to_string()));
        assert_eq!(log.last().map(String::as_str), Some("QUIT"));
    }

    #[test]
    fn ftp_upload_pasv_fallback() {
        let fake = fake_server(&[], false);
        ftp_upload(&settings(fake.port, ""), "pa ss", "x.png", b"hello").unwrap();
        fake.handle.join().unwrap();
        let (path, bytes) = fake.stored.lock().unwrap().take().unwrap();
        assert_eq!(path, "/x.png");
        assert_eq!(bytes, b"hello");
        assert!(fake.log.lock().unwrap().contains(&"PASV".to_string()));
    }

    #[test]
    fn ftp_bad_password() {
        let fake = fake_server(&[], true);
        let e = ftp_upload(&settings(fake.port, "/"), "wrong", "x.png", b"x").unwrap_err();
        assert!(format!("{e:#}").contains("login failed"), "{e:#}");
        drop(fake);
    }

    #[test]
    fn reply_parsers() {
        assert_eq!(parse_epsv("Entering Extended Passive Mode (|||6446|)"), Some(6446));
        assert_eq!(parse_epsv("ok (!!!21!)"), Some(21));
        assert_eq!(parse_epsv("nothing"), None);
        assert_eq!(parse_pasv("Entering Passive Mode (192,168,1,2,19,137)."), Some(([192, 168, 1, 2], 5001)));
        assert_eq!(parse_pasv("Entering Passive Mode (1,2,3)"), None);
    }

    #[test]
    fn urls() {
        assert_eq!(public_url("", "a.png"), None);
        assert_eq!(public_url("https://x.io/s", "a b+c.png").as_deref(), Some("https://x.io/s/a%20b%2Bc.png"));
        assert_eq!(public_url("https://x.io/s/", "ä.png").as_deref(), Some("https://x.io/s/%C3%A4.png"));
    }

    #[test]
    fn sftp_script_and_args() {
        let s = sftp_script("/var/www/shots", Path::new(r"C:\Users\me\Pictures\a b.png"), "a b.png");
        assert_eq!(
            s,
            "-mkdir \"/var\"\n-mkdir \"/var/www\"\n-mkdir \"/var/www/shots\"\ncd \"/var/www/shots/\"\n\
             put \"C:/Users/me/Pictures/a b.png\" \"a b.png\"\nbye\n"
        );
        assert_eq!(sftp_script("", Path::new("x.png"), "x.png"), "put \"x.png\" \"x.png\"\nbye\n");
        let mut st = settings(0, "/");
        st.protocol = UploadProtocol::Sftp;
        st.sftp_key_path = r"C:\keys\id_ed25519".into();
        let a = sftp_args(&st, Path::new(r"C:\t\s.txt"));
        assert_eq!(a[0..2], ["-b".to_string(), r"C:\t\s.txt".to_string()]);
        assert!(a.contains(&"-oBatchMode=yes".to_string()));
        assert!(a.contains(&"-oStrictHostKeyChecking=accept-new".to_string()));
        let p = a.iter().position(|x| x == "-P").unwrap();
        assert_eq!(a[p + 1], "22");
        let i = a.iter().position(|x| x == "-i").unwrap();
        assert_eq!(a[i + 1], r"C:\keys\id_ed25519");
        assert_eq!(a.last().map(String::as_str), Some("bob@127.0.0.1"));
    }

    #[test]
    fn validation_errors() {
        let st = UploadSettings::default();
        assert!(upload_file(Path::new("x.png"), &st).is_err());
        let st = settings(21, "/");
        assert!(upload_file(Path::new(r"C:\missing\x.png"), &st).is_err());
    }
}
