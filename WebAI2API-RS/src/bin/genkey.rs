//! 生成 API 密钥，对应原 `scripts/genkey.js`：`sk-` + 48 位十六进制。

fn main() {
    let mut bytes = [0u8; 24];
    getrandom(bytes.as_mut_slice());
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    println!("sk-{hex}");
    println!("请将以上密钥复制到 data/config.yaml 的 server.auth");
}

/// 用系统随机源填充缓冲区。
fn getrandom(buf: &mut [u8]) {
    use std::io::Read;
    let mut f = std::fs::File::open("/dev/urandom").expect("无法打开 /dev/urandom");
    f.read_exact(buf).expect("读取随机数失败");
}
