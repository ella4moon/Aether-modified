// Used by CI/local validation with the pinned sing-box binary. No interface is
// created by `sing-box check`. Windows paths deliberately exercise escaping.
fn main() {
    let request = aether_whole_laptop::Request {
        local_addr: "127.0.0.1:1819".parse().unwrap(),
        udp_enabled: std::env::args().any(|a| a == "--udp"),
        engine_path: r"C:\Program Files\Aether-GUI\binaries\aether.exe".into(),
        sidecar_path: r"C:\Program Files\Aether-GUI\binaries\sing-box.exe".into(),
        data_dir: r"C:\Users\test\AppData\Roaming\com.cluvexstudio.aethergui".into(),
    };
    println!("{}", aether_whole_laptop::config(&request).unwrap());
}
