//! v0.44.0: Container Sandbox Spec + REAL Docker orchestration (pi-mono inspired)
//!
//! 灵感: pi-mono `packages/coding-agent/docs/containerization.md` (3 patterns)
//! - **Gondolin**: micro-VM (host runs agent, VM runs tools) — future
//! - **Plain Docker**: full container via `docker run` — v0.44.0 ✅
//! - **OpenShell**: policy-controlled sandbox — future
//!
//! v0.44.0: REAL Docker implementation (NOT metadata-only!)
//! - `sandbox.containerize("docker", mounts=[...], ...)` → spawns
//!   `docker run -d --name mora-<uuid> <mounts> <image> sleep infinity`
//! - Returns REAL container ID (`docker ps` can verify)
//! - `sandbox.container_exec(cmd)` → runs cmd inside container via `docker exec`
//! - `sandbox.container_clear()` → `docker rm -f` (kill + remove)

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// v0.44.0: 容器后端类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContainerBackend {
    /// Docker: full container via `docker run` (v0.44.0 first-class)
    Docker,
    /// Gondolin: micro-VM (future v1.0+)
    Gondolin,
    /// OpenShell: policy-controlled sandbox (future v1.0+)
    OpenShell,
}

impl ContainerBackend {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "docker" => Some(Self::Docker),
            "gondolin" => Some(Self::Gondolin),
            "openshell" | "open_shell" => Some(Self::OpenShell),
            _ => None,
        }
    }
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Docker => "docker",
            Self::Gondolin => "gondolin",
            Self::OpenShell => "openshell",
        }
    }

    /// v0.44.0: 这个 backend 在 v0.44.0 是否真的实现 (vs metadata-only / future)
    pub fn is_implemented_v044(&self) -> bool {
        matches!(self, Self::Docker)
    }
}

/// v0.44.0: 网络隔离模式
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkMode {
    /// 容器完全无网络 (最严格)
    Isolated,
    /// 允许访问 host 网络 (无隔离)
    Host,
}

impl NetworkMode {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "isolated" | "none" => Some(Self::Isolated),
            "host" => Some(Self::Host),
            _ => None,
        }
    }
}

/// v0.44.0: 挂载配置 (host_path:container_path[:mode])
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MountSpec {
    pub host_path: String,
    pub container_path: String,
    /// "ro" or "rw"
    pub mode: String,
}

impl MountSpec {
    pub fn parse(s: &str) -> Result<Self, String> {
        // splitn(3, ':') 允许 path 含 ':' (最后一个 ':mode' 可选)
        let parts: Vec<&str> = s.splitn(3, ':').collect();
        match parts.len() {
            2 => Ok(Self {
                host_path: parts[0].to_string(),
                container_path: parts[1].to_string(),
                mode: "rw".to_string(),
            }),
            3 => Ok(Self {
                host_path: parts[0].to_string(),
                container_path: parts[1].to_string(),
                mode: parts[2].to_string(),
            }),
            _ => Err(format!(
                "mount spec must be 'host:container[:mode]', got: {}",
                s
            )),
        }
    }

    /// 渲染成 `docker run -v` 参数 (e.g. "/data:/data:ro")
    pub fn to_docker_arg(&self) -> String {
        format!("{}:{}:{}", self.host_path, self.container_path, self.mode)
    }
}

/// v0.44.0: 资源限制 (best-effort, metadata only)
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResourceLimits {
    /// CPU 核心数 (None = unlimited)
    pub cpu_cores: Option<u32>,
    /// 内存上限 (MB, None = unlimited)
    pub memory_mb: Option<u64>,
}

/// v0.44.0: 容器规格 (用户意图)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainerSpec {
    pub backend: ContainerBackend,
    pub mounts: Vec<MountSpec>,
    pub network: NetworkMode,
    pub limits: ResourceLimits,
    /// Docker 镜像 (v0.44.0 only used for Docker backend)
    pub image: String,
}

impl ContainerSpec {
    pub fn new(backend: ContainerBackend) -> Self {
        Self {
            backend,
            mounts: Vec::new(),
            network: NetworkMode::Isolated,
            limits: ResourceLimits::default(),
            // 默认 image: alpine (轻量, sleep 命令自带)
            image: "alpine:latest".to_string(),
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        for mount in &self.mounts {
            if mount.host_path.is_empty() {
                return Err("mount.host_path cannot be empty".to_string());
            }
            if mount.container_path.is_empty() {
                return Err("mount.container_path cannot be empty".to_string());
            }
            if mount.mode != "ro" && mount.mode != "rw" {
                return Err(format!(
                    "mount.mode must be 'ro' or 'rw', got: {}",
                    mount.mode
                ));
            }
        }
        if self.image.is_empty() {
            return Err("container.image cannot be empty".to_string());
        }
        Ok(())
    }

    /// 渲染成 `docker run` 命令参数 (除 image / name 之外)
    fn to_docker_run_args(&self, container_name: &str) -> Vec<String> {
        let mut args = vec![
            "run".to_string(),
            "-d".to_string(),
            "--name".to_string(),
            container_name.to_string(),
        ];

        // mounts
        for mount in &self.mounts {
            args.push("-v".to_string());
            args.push(mount.to_docker_arg());
        }

        // network
        match self.network {
            NetworkMode::Isolated => args.push("--network=none".to_string()),
            NetworkMode::Host => {} // 默认就是 host network
        }

        // resource limits
        if let Some(cores) = self.limits.cpu_cores {
            args.push(format!("--cpus={}", cores));
        }
        if let Some(mem) = self.limits.memory_mb {
            args.push(format!("--memory={}m", mem));
        }

        // keep-alive command (container 需要前台运行, 我们用 sleep infinity)
        args.push(self.image.clone());
        args.push("sleep".to_string());
        args.push("infinity".to_string());
        args
    }
}

/// v0.44.0: 容器运行时 handle (保存真实 container ID + process info)
#[derive(Debug, Clone)]
pub struct ContainerHandle {
    pub container_id: String,
    pub container_name: String,
    pub backend: ContainerBackend,
    pub spec: ContainerSpec,
    pub started_at: Instant,
}

impl ContainerHandle {
    pub fn new(container_id: String, container_name: String, spec: ContainerSpec) -> Self {
        Self {
            container_id,
            container_name,
            backend: spec.backend,
            spec,
            started_at: Instant::now(),
        }
    }

    /// `docker exec <id> <cmd>` — 在容器内执行命令, 返回 (exit_code, stdout, stderr)
    pub fn exec(&self, cmd: &[&str]) -> Result<(i32, String, String), String> {
        let mut args = vec!["exec".to_string(), self.container_id.clone()];
        args.extend(cmd.iter().map(|s| s.to_string()));
        let output = Command::new("docker")
            .args(&args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .map_err(|e| format!("docker exec spawn failed: {}", e))?;
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        let code = output.status.code().unwrap_or(-1);
        Ok((code, stdout, stderr))
    }

    /// `docker rm -f <id>` — 强制删除 (kill + remove)
    pub fn destroy(&self) -> Result<(), String> {
        let output = Command::new("docker")
            .args(["rm", "-f", &self.container_id])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .map_err(|e| format!("docker rm spawn failed: {}", e))?;
        if !output.status.success() {
            return Err(format!(
                "docker rm -f failed: {}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        Ok(())
    }

    pub fn elapsed(&self) -> Duration {
        self.started_at.elapsed()
    }
}

/// v0.44.0: `docker run` 生成 container_name = "mora-<uuid>"
pub fn generate_container_name() -> String {
    let id: u64 = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    format!("mora-{:x}", id)
}

/// v0.44.0: 真实 spawn docker container
pub fn spawn_container(spec: &ContainerSpec) -> Result<ContainerHandle, String> {
    spec.validate()?;

    if !spec.backend.is_implemented_v044() {
        return Err(format!(
            "backend '{}' not yet implemented in v0.44.0 (only Docker supported; \
             Gondolin/OpenShell deferred to v1.0+)",
            spec.backend.as_str()
        ));
    }

    // 检查 docker CLI 可用
    let which = Command::new("docker")
        .arg("version")
        .arg("--format")
        .arg("{{.Server.Version}}")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("docker CLI not found: {}", e))?;
    if !which.status.success() {
        return Err(format!(
            "docker daemon unreachable: {}",
            String::from_utf8_lossy(&which.stderr)
        ));
    }

    let name = generate_container_name();
    let args = spec.to_docker_run_args(&name);

    let output = Command::new("docker")
        .args(&args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("docker run spawn failed: {}", e))?;

    if !output.status.success() {
        return Err(format!(
            "docker run failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    let container_id = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if container_id.is_empty() {
        return Err("docker run returned empty container ID".to_string());
    }

    Ok(ContainerHandle::new(container_id, name, spec.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn container_backend_parse_roundtrip() {
        for backend in [
            ContainerBackend::Docker,
            ContainerBackend::Gondolin,
            ContainerBackend::OpenShell,
        ] {
            assert_eq!(ContainerBackend::parse(backend.as_str()), Some(backend));
        }
        assert_eq!(ContainerBackend::parse("unknown"), None);
    }

    #[test]
    fn network_mode_parse() {
        assert_eq!(NetworkMode::parse("isolated"), Some(NetworkMode::Isolated));
        assert_eq!(NetworkMode::parse("host"), Some(NetworkMode::Host));
        assert_eq!(NetworkMode::parse("none"), Some(NetworkMode::Isolated));
        assert_eq!(NetworkMode::parse("garbage"), None);
    }

    #[test]
    fn mount_spec_parse() {
        let m = MountSpec::parse("/data:/container/data:ro").unwrap();
        assert_eq!(m.host_path, "/data");
        assert_eq!(m.container_path, "/container/data");
        assert_eq!(m.mode, "ro");

        let m2 = MountSpec::parse("/data:/data").unwrap();
        assert_eq!(m2.mode, "rw"); // default

        assert!(MountSpec::parse("no_colon").is_err()); // 缺 separator
        let m3 = MountSpec::parse("a:b:c").unwrap();
        assert_eq!(m3.host_path, "a");
        assert_eq!(m3.container_path, "b");
        assert_eq!(m3.mode, "c");
    }

    #[test]
    fn mount_spec_to_docker_arg() {
        let m = MountSpec::parse("/data:/data:ro").unwrap();
        assert_eq!(m.to_docker_arg(), "/data:/data:ro");
    }

    #[test]
    fn container_spec_validate_rejects_empty_paths() {
        let mut spec = ContainerSpec::new(ContainerBackend::Docker);
        spec.mounts.push(MountSpec {
            host_path: "".to_string(),
            container_path: "/c".to_string(),
            mode: "rw".to_string(),
        });
        assert!(spec.validate().is_err());
    }

    #[test]
    fn container_spec_validate_rejects_bad_mode() {
        let mut spec = ContainerSpec::new(ContainerBackend::Docker);
        spec.mounts.push(MountSpec {
            host_path: "/h".to_string(),
            container_path: "/c".to_string(),
            mode: "xx".to_string(),
        });
        assert!(spec.validate().is_err());
    }

    #[test]
    fn container_spec_validate_rejects_empty_image() {
        let mut spec = ContainerSpec::new(ContainerBackend::Docker);
        spec.image = "".to_string();
        assert!(spec.validate().is_err());
    }

    #[test]
    fn container_spec_default_is_isolated() {
        let spec = ContainerSpec::new(ContainerBackend::Docker);
        assert_eq!(spec.network, NetworkMode::Isolated);
        assert!(spec.mounts.is_empty());
        assert_eq!(spec.limits.cpu_cores, None);
        assert_eq!(spec.image, "alpine:latest");
    }

    #[test]
    fn docker_run_args_render() {
        let mut spec = ContainerSpec::new(ContainerBackend::Docker);
        spec.mounts
            .push(MountSpec::parse("/data:/data:ro").unwrap());
        spec.network = NetworkMode::Isolated;
        spec.limits.cpu_cores = Some(2);
        spec.limits.memory_mb = Some(512);
        let args = spec.to_docker_run_args("mora-test");
        assert!(args.contains(&"run".to_string()));
        assert!(args.contains(&"--name".to_string()));
        assert!(args.contains(&"mora-test".to_string()));
        assert!(args.contains(&"-v".to_string()));
        assert!(args.contains(&"/data:/data:ro".to_string()));
        assert!(args.contains(&"--network=none".to_string()));
        assert!(args.contains(&"--cpus=2".to_string()));
        assert!(args.contains(&"--memory=512m".to_string()));
        assert!(args.contains(&"alpine:latest".to_string()));
        assert!(args.contains(&"sleep".to_string()));
        assert!(args.contains(&"infinity".to_string()));
    }

    #[test]
    fn generate_container_name_is_unique() {
        let n1 = generate_container_name();
        std::thread::sleep(std::time::Duration::from_millis(2));
        let n2 = generate_container_name();
        assert_ne!(n1, n2);
        assert!(n1.starts_with("mora-"));
        assert!(n2.starts_with("mora-"));
    }

    #[test]
    fn unimplemented_backends_return_error() {
        let mut spec = ContainerSpec::new(ContainerBackend::Gondolin);
        spec.image = "alpine:latest".to_string();
        let err = spawn_container(&spec).unwrap_err();
        assert!(err.contains("not yet implemented"));
    }

    /// v0.44.0 真实 docker spawn 集成测试 (requires Docker daemon)
    /// 默认忽略（无 docker CI 时不强制要求）
    #[test]
    #[ignore]
    fn real_docker_spawn_and_destroy() {
        let spec = ContainerSpec::new(ContainerBackend::Docker);
        let handle = spawn_container(&spec).expect("spawn must succeed");
        assert!(!handle.container_id.is_empty());
        assert!(
            handle.container_id.len() >= 12,
            "container ID hex length >= 12"
        );

        // 验证 container 真的在运行
        let (code, stdout, _) = handle
            .exec(&["echo", "hello-from-mora"])
            .expect("docker exec must work");
        assert_eq!(code, 0);
        assert!(stdout.contains("hello-from-mora"));

        // 清理
        handle.destroy().expect("docker rm must succeed");
    }
}
