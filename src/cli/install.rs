//! v0.110.1: `mora install <url>` —— 从 URL 安装 Mora 包到 `vendor/` 并登记 `mora.lock`。
//!
//! ## 契约
//!
//! - **只认 2xx**：HTTP 状态码非 2xx 一律视为失败 —— 不落地任何文件、不写锁文件。
//! - **原子落地**：先写 `<dest>.part` 暂存文件，状态校验通过后才 `rename` 到目标路径。
//! - **失败即清理**：失败路径上删除暂存文件，绝不留下半个包。
//! - **退出码**：成功 0 / 失败非 0（与 CLI 其余子命令一致）。
//!
//! ## v0.110.1 修正：HTTP 状态此前**完全没有被校验**
//!
//! 旧实现直接 `curl -L -o dest url`，而 **curl 默认不因 HTTP 错误失败**：
//! 404 / 502 的响应体（错误页 HTML）会被原样写进 `-o` 指向的文件，
//! curl 退出码仍是 **0** ⇒ CLI 打印 `Installed`、写入 `mora.lock` ——
//! **404/502 被当成安装成功**，错误页以「已安装的包」形态留在 `vendor/` 里。
//!
//! 现在由 [`download_to`] 取出真实状态码并交给 [`status_is_success`] 判定：
//! 非 2xx 一律失败，且暂存文件被删除 —— 错误页不再可能变成「已安装的包」。
//!
//! ## 工具选择
//!
//! 优先 `curl`（Windows 10+ 与各类 Unix 均自带），回退 `wget`。
//! 两条路径都只把**成功**视为成功：
//! - curl：`-w "%{http_code}"` 输出真实状态码（`-f` 同时禁止错误响应体落盘）；
//! - wget：HTTP 错误时退出码非 0，状态码从 stderr 的 `ERROR 404: Not Found.` 解析。

use std::fs;
use std::process::Command;

/// 安装一个包。返回进程退出码（成功 0，失败非 0）。
pub fn install_package(url: &str) -> i32 {
    let vendor_dir = "vendor";
    if let Err(e) = fs::create_dir_all(vendor_dir) {
        eprintln!("Failed to create vendor directory: {e}");
        return 1;
    }

    let pkg_name = package_name_from_url(url);
    let dest = format!("{vendor_dir}/{pkg_name}.mora");
    let staging = format!("{dest}.part");

    println!("Installing {pkg_name} from {url}...");

    match download_to(url, &staging) {
        // 拿到状态码却不是 2xx ⇒ 失败（404/502 等）。
        Ok(Some(code)) if !status_is_success(code) => {
            discard_staging(&staging);
            eprintln!("Failed to install {pkg_name}: HTTP {code} from {url}");
            1
        }
        // 2xx（或工具未报告状态码但传输成功）⇒ 落地。
        Ok(_) => match fs::rename(&staging, &dest) {
            Ok(()) => {
                println!("Installed {pkg_name} -> {dest}");
                update_lock(pkg_name, url);
                0
            }
            Err(e) => {
                discard_staging(&staging);
                eprintln!("Failed to install {pkg_name}: cannot write {dest}: {e}");
                1
            }
        },
        // 传输层失败或 HTTP 错误（wget 路径）。
        Err(msg) => {
            discard_staging(&staging);
            eprintln!("Failed to install {pkg_name}: {msg}");
            1
        }
    }
}

/// 从 URL 推导包名：取最后一段路径，去掉 `.mora` 后缀与查询串/片段。
///
/// `http://host/pkg.mora?token=abc` ⇒ `pkg`（否则会落盘成
/// `vendor/pkg.mora?token=abc.mora`）。
fn package_name_from_url(url: &str) -> &str {
    let without_query = url.split(['?', '#']).next().unwrap_or(url);
    let last = without_query.rsplit('/').next().unwrap_or(without_query);
    let name = last.strip_suffix(".mora").unwrap_or(last);
    if name.is_empty() { "package" } else { name }
}

/// HTTP 状态码是否代表成功（2xx）。
///
/// 3xx **不算成功**：curl `-L` 会跟随重定向，能落到这里的 3xx 说明重定向未被跟随
/// 或被服务端拒绝，此时响应体不是包。
fn status_is_success(code: u16) -> bool {
    (200..300).contains(&code)
}

/// 解析 curl `-w "%{http_code}"` 的输出。
///
/// `000` 是 curl 在**传输层失败**时的占位（没有真实状态码），与空输出一样返回 `None`。
fn parse_http_status(stdout: &str) -> Option<u16> {
    let text = stdout.trim();
    if text.is_empty() {
        return None;
    }
    let code: u16 = text.parse().ok()?;
    if code == 0 { None } else { Some(code) }
}

/// 从 wget 的 stderr（`ERROR 404: Not Found.`）解析 HTTP 状态码。
fn parse_wget_error_status(stderr: &str) -> Option<u16> {
    let mut rest = stderr;
    while let Some(idx) = rest.find("ERROR ") {
        let after = &rest[idx + "ERROR ".len()..];
        let digits: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
        if let Ok(code) = digits.parse::<u16>()
            && (100..600).contains(&code)
        {
            return Some(code);
        }
        rest = after;
    }
    None
}

/// 下载到 `dest`。
///
/// - `Ok(Some(code))`：拿到真实 HTTP 状态码（由调用方判定是否 2xx）；
/// - `Ok(None)`：传输成功但工具未报告状态码（wget 成功路径）；
/// - `Err(msg)`：传输失败或 HTTP 错误。
fn download_to(url: &str, dest: &str) -> Result<Option<u16>, String> {
    if command_exists("curl") {
        download_with_curl(url, dest)
    } else if command_exists("wget") {
        download_with_wget(url, dest)
    } else {
        Err(
            "neither curl nor wget found; install one of them, or download the package manually"
                .to_string(),
        )
    }
}

/// curl 路径：`-L` 跟随重定向、`-f` 禁止错误响应体落盘、`-w` 输出真实状态码。
fn download_with_curl(url: &str, dest: &str) -> Result<Option<u16>, String> {
    let output = Command::new("curl")
        .args(["-L", "-sS", "-f", "-o", dest, "-w", "%{http_code}", url])
        .output()
        .map_err(|e| format!("failed to run curl: {e}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    match parse_http_status(&stdout) {
        Some(code) => Ok(Some(code)),
        None if output.status.success() => Ok(None),
        None => Err(describe_failure("curl", &stderr, output.status.code())),
    }
}

/// wget 路径：HTTP 错误时退出码非 0，状态码从 stderr 解析。
fn download_with_wget(url: &str, dest: &str) -> Result<Option<u16>, String> {
    let output = Command::new("wget")
        .args(["-O", dest, url])
        .output()
        .map_err(|e| format!("failed to run wget: {e}"))?;
    if output.status.success() {
        return Ok(None);
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    match parse_wget_error_status(&stderr) {
        Some(code) => Err(format!("HTTP {code}")),
        None => Err(describe_failure("wget", &stderr, output.status.code())),
    }
}

/// 把工具失败信息整理成一句可读说明。
fn describe_failure(tool: &str, stderr: &str, exit_code: Option<i32>) -> String {
    let detail = stderr.trim();
    if detail.is_empty() {
        match exit_code {
            Some(code) => format!("{tool} exited with code {code}"),
            None => format!("{tool} failed"),
        }
    } else {
        format!("{tool}: {detail}")
    }
}

/// 失败路径：删除暂存文件（错误页不得留在磁盘上）。
fn discard_staging(staging: &str) {
    let _ = fs::remove_file(staging);
}

/// 登记到 `mora.lock`（仅在安装成功后调用）。
fn update_lock(pkg_name: &str, url: &str) {
    let lock_path = "mora.lock";
    let mut content = String::new();
    if std::path::Path::new(lock_path).exists() {
        content = fs::read_to_string(lock_path).unwrap_or_default();
    }
    let entry = format!("{pkg_name} = \"{url}\"\n");
    if !content.contains(pkg_name) {
        content.push_str(&entry);
        if let Err(e) = fs::write(lock_path, content) {
            eprintln!("Warning: failed to update {lock_path}: {e}");
        }
    }
}

/// 工具是否在 PATH 上（Windows 用 `where`，Unix 用 `which`）。
fn command_exists(cmd: &str) -> bool {
    #[cfg(target_os = "windows")]
    {
        Command::new("where")
            .arg(cmd)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
    #[cfg(not(target_os = "windows"))]
    {
        Command::new("which")
            .arg(cmd)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_name_strips_path_suffix_and_query() {
        assert_eq!(package_name_from_url("http://h/pkg.mora"), "pkg");
        assert_eq!(package_name_from_url("http://h/dir/pkg.mora"), "pkg");
        assert_eq!(package_name_from_url("http://h/pkg.mora?token=abc"), "pkg");
        assert_eq!(package_name_from_url("http://h/pkg.mora#frag"), "pkg");
        assert_eq!(package_name_from_url("http://h/pkg"), "pkg");
        assert_eq!(package_name_from_url("http://h/"), "package");
    }

    #[test]
    fn only_2xx_counts_as_success() {
        assert!(status_is_success(200));
        assert!(status_is_success(201));
        assert!(status_is_success(204));
        assert!(status_is_success(299));
        // 3xx 不算成功：重定向没被跟随时响应体不是包。
        assert!(!status_is_success(301));
        assert!(!status_is_success(302));
        assert!(!status_is_success(400));
        assert!(!status_is_success(404));
        assert!(!status_is_success(500));
        assert!(!status_is_success(502));
    }

    #[test]
    fn curl_status_parsing() {
        assert_eq!(parse_http_status("200"), Some(200));
        assert_eq!(parse_http_status("404\n"), Some(404));
        assert_eq!(parse_http_status(" 502 "), Some(502));
        // 传输层失败时 curl 打印 000 —— 没有真实状态码。
        assert_eq!(parse_http_status("000"), None);
        assert_eq!(parse_http_status(""), None);
        assert_eq!(parse_http_status("not-a-code"), None);
    }

    #[test]
    fn wget_error_status_parsing() {
        assert_eq!(parse_wget_error_status("ERROR 404: Not Found."), Some(404));
        assert_eq!(
            parse_wget_error_status("ERROR 502: Bad Gateway.\n"),
            Some(502)
        );
        assert_eq!(parse_wget_error_status("Resolving example.com"), None);
        assert_eq!(parse_wget_error_status(""), None);
    }
}
