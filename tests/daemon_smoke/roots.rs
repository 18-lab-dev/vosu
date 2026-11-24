struct ChildGuard(Option<Child>);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Some(child) = self.0.as_mut() {
            let _ = child.kill();
            let _ = vosu_proptest::wait::child(child);
        }
    }
}

fn available_port() -> u16 {
    TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn legacy_config(port: u16, static_root: &Path) -> String {
    format!(
        "ENABLE_AUTOUPDATE=false\nOPEN_DASHBOARD_ON_STARTUP=false\nDEBUG_LOG=false\nPOLL_RATE=150\nPRECISE_DATA_POLL_RATE=10\nSERVER_IP=127.0.0.1\nSERVER_PORT={port}\nDASHBOARD_IP=127.0.0.1\nDASHBOARD_PORT=0\nSTATIC_FOLDER_PATH={}\nALLOWED_IPS=127.0.0.1,localhost,absolute\n",
        static_root.display(),
    )
}

fn toml_config(port: u16, static_root: &Path) -> String {
    let static_root = toml::Value::String(static_root.to_string_lossy().into_owned());
    format!(
        "ENABLE_AUTOUPDATE = false\nOPEN_DASHBOARD_ON_STARTUP = false\nDEBUG_LOG = false\nPOLL_RATE = 150\nPRECISE_DATA_POLL_RATE = 10\nSERVER_IP = \"127.0.0.1\"\nSERVER_PORT = {port}\nDASHBOARD_IP = \"127.0.0.1\"\nDASHBOARD_PORT = 0\nSTATIC_FOLDER_PATH = {static_root}\nALLOWED_IPS = \"127.0.0.1,localhost,absolute\"\n"
    )
}

fn smoke_channel() -> vosu_sys::platform::BuildChannel {
    let binary = std::env::var_os("VOSU_RELEASE_SMOKE_BINARY")
        .unwrap_or_else(|| env!("CARGO_BIN_EXE_vosu").into());
    let mut child = Command::new(binary)
        .arg("--version")
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let status = vosu_proptest::wait::child(&mut child).unwrap();
    assert!(status.success());
    let mut version = String::new();
    std::io::Read::read_to_string(&mut child.stdout.take().unwrap(), &mut version).unwrap();
    vosu_sys::platform::BuildChannel::from_version(version.split_whitespace().last().unwrap()).unwrap()
}
