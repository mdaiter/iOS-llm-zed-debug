use serde_json::{json, Value};
use zed_extension_api::{
    register_extension, DebugAdapterBinary, DebugConfig, DebugRequest, DebugScenario,
    DebugTaskDefinition, EnvVars, Extension, StartDebuggingRequestArguments,
    StartDebuggingRequestArgumentsRequest, Worktree,
};

const ADAPTER_NAME: &str = "ios-lldb";
const CONFIG_ENV_VAR: &str = "IOS_LLDB_DAP_CONFIG";

pub struct IosLldbExtension;

impl Extension for IosLldbExtension {
    fn new() -> Self {
        Self
    }

    fn get_dap_binary(
        &mut self,
        adapter_name: String,
        config: DebugTaskDefinition,
        user_provided_debug_adapter_path: Option<String>,
        worktree: &Worktree,
    ) -> Result<DebugAdapterBinary, String> {
        ensure_adapter(&adapter_name)?;
        build_debug_adapter_binary(&config, user_provided_debug_adapter_path, worktree)
    }

    fn dap_request_kind(
        &mut self,
        _adapter_name: String,
        config: Value,
    ) -> Result<StartDebuggingRequestArgumentsRequest, String> {
        request_kind_from_config(&config)
    }

    fn dap_config_to_scenario(&mut self, config: DebugConfig) -> Result<DebugScenario, String> {
        convert_debug_config(config)
    }
}

register_extension!(IosLldbExtension);

fn ensure_adapter(adapter: &str) -> Result<(), String> {
    if adapter == ADAPTER_NAME {
        Ok(())
    } else {
        Err(format!("unsupported adapter: {adapter}"))
    }
}

fn build_debug_adapter_binary<W: WorktreeLike>(
    task: &DebugTaskDefinition,
    user_path: Option<String>,
    worktree: &W,
) -> Result<DebugAdapterBinary, String> {
    let config_json: Value =
        serde_json::from_str(&task.config).map_err(|err| format!("invalid config: {err}"))?;
    let request_kind = request_kind_from_config(&config_json)?;
    let command = resolve_binary_path(worktree, user_path)?;
    let mut env = worktree.shell_env();
    upsert_env(&mut env, CONFIG_ENV_VAR, task.config.clone());

    Ok(DebugAdapterBinary {
        command: Some(command),
        arguments: Vec::new(),
        envs: env,
        cwd: None,
        connection: None,
        request_args: StartDebuggingRequestArguments {
            configuration: task.config.clone(),
            request: request_kind,
        },
    })
}

fn resolve_binary_path<W: WorktreeLike>(
    worktree: &W,
    user_path: Option<String>,
) -> Result<String, String> {
    if let Some(path) = user_path {
        return Ok(path);
    }

    worktree
        .which(ADAPTER_NAME)
        .ok_or_else(|| format!("unable to find `{ADAPTER_NAME}` on PATH"))
}

fn upsert_env(env: &mut EnvVars, key: &str, value: String) {
    if let Some(slot) = env.iter_mut().find(|(existing, _)| existing == key) {
        slot.1 = value;
        return;
    }
    env.push((key.to_string(), value));
}

fn request_kind_from_config(
    config: &Value,
) -> Result<StartDebuggingRequestArgumentsRequest, String> {
    match config
        .get("request")
        .and_then(Value::as_str)
        .unwrap_or("launch")
    {
        "launch" => Ok(StartDebuggingRequestArgumentsRequest::Launch),
        "attach" => Ok(StartDebuggingRequestArgumentsRequest::Attach),
        other => Err(format!("unknown request kind `{other}`")),
    }
}

fn convert_debug_config(config: DebugConfig) -> Result<DebugScenario, String> {
    let body = match config.request {
        DebugRequest::Launch(launch) => json!({
            "request": "launch",
            "program": launch.program,
            "args": launch.args,
            "cwd": launch.cwd,
            "env": env_list_to_value(launch.envs),
            "debugserverPort": 0,
            "stopOnEntry": config.stop_on_entry.unwrap_or(false),
        }),
        DebugRequest::Attach(attach) => json!({
            "request": "attach",
            "processId": attach.process_id,
            "debugserverPort": 0,
            "stopOnEntry": config.stop_on_entry.unwrap_or(false),
        }),
    };

    Ok(DebugScenario {
        label: config.label,
        adapter: ADAPTER_NAME.to_string(),
        build: None,
        config: serde_json::to_string(&body).map_err(|err| err.to_string())?,
        tcp_connection: None,
    })
}

fn env_list_to_value(envs: EnvVars) -> Value {
    let map = envs
        .into_iter()
        .map(|(k, v)| (k, Value::String(v)))
        .collect::<serde_json::Map<_, _>>();
    Value::Object(map)
}

trait WorktreeLike {
    fn which(&self, binary_name: &str) -> Option<String>;
    fn shell_env(&self) -> EnvVars;
}

impl WorktreeLike for Worktree {
    fn which(&self, binary_name: &str) -> Option<String> {
        Worktree::which(self, binary_name)
    }

    fn shell_env(&self) -> EnvVars {
        Worktree::shell_env(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zed_extension_api::{AttachRequest, LaunchRequest, TcpArgumentsTemplate};

    #[test]
    fn dap_request_kind_uses_request_field() {
        let mut extension = IosLldbExtension;
        let launch = json!({ "request": "launch" });
        assert_eq!(
            extension
                .dap_request_kind(ADAPTER_NAME.to_string(), launch)
                .unwrap(),
            StartDebuggingRequestArgumentsRequest::Launch
        );

        let attach = json!({ "request": "attach" });
        assert_eq!(
            extension
                .dap_request_kind(ADAPTER_NAME.to_string(), attach)
                .unwrap(),
            StartDebuggingRequestArgumentsRequest::Attach
        );
    }

    #[test]
    fn get_dap_binary_prefers_user_path() {
        let config = sample_task_definition();
        let mut worktree = FakeWorktree::new();
        worktree.set_binary_path("/usr/bin/ios-lldb-dap");
        let binary =
            build_debug_adapter_binary(&config, Some("/custom/dap".into()), &worktree).unwrap();
        assert_eq!(binary.command.as_deref(), Some("/custom/dap"));
        assert!(binary
            .envs
            .iter()
            .any(|(key, value)| key == CONFIG_ENV_VAR && value.contains("debugserverPort")));
    }

    #[test]
    fn get_dap_binary_uses_worktree_path_when_not_overridden() {
        let config = sample_task_definition();
        let mut worktree = FakeWorktree::new();
        worktree.set_binary_path("/usr/local/bin/ios-lldb-dap");
        let binary = build_debug_adapter_binary(&config, None, &worktree).unwrap();
        assert_eq!(
            binary.command.as_deref(),
            Some("/usr/local/bin/ios-lldb-dap")
        );
    }

    fn sample_task_definition() -> DebugTaskDefinition {
        DebugTaskDefinition {
            label: "Demo".into(),
            adapter: ADAPTER_NAME.into(),
            config:
                r#"{"request":"launch","debugserverPort":12345,"program":"/tmp/a","cwd":"/tmp"}"#
                    .into(),
            tcp_connection: Some(TcpArgumentsTemplate {
                port: None,
                host: None,
                timeout: None,
            }),
        }
    }

    struct FakeWorktree {
        binary_path: Option<String>,
        env: EnvVars,
    }

    impl FakeWorktree {
        fn new() -> Self {
            Self {
                binary_path: None,
                env: vec![("PATH".into(), "/tmp".into())],
            }
        }

        fn set_binary_path(&mut self, path: &str) {
            self.binary_path = Some(path.into());
        }
    }

    impl WorktreeLike for FakeWorktree {
        fn which(&self, binary_name: &str) -> Option<String> {
            if self.binary_path.is_some() && binary_name == ADAPTER_NAME {
                self.binary_path.clone()
            } else {
                None
            }
        }

        fn shell_env(&self) -> EnvVars {
            self.env.clone()
        }
    }

    #[test]
    fn convert_debug_config_produces_scenario() {
        let config = DebugConfig {
            label: "Demo".into(),
            adapter: ADAPTER_NAME.into(),
            request: DebugRequest::Launch(LaunchRequest {
                program: "/bin/app".into(),
                cwd: Some("/tmp".into()),
                args: vec!["--flag".into()],
                envs: vec![("RUST_LOG".into(), "info".into())],
            }),
            stop_on_entry: Some(true),
        };

        let scenario = convert_debug_config(config).unwrap();
        assert_eq!(scenario.adapter, ADAPTER_NAME);
        assert!(
            scenario.config.contains(r#""request":"launch""#),
            "config should serialize launch request"
        );
    }

    #[test]
    fn convert_debug_config_handles_attach() {
        let config = DebugConfig {
            label: "Demo".into(),
            adapter: ADAPTER_NAME.into(),
            request: DebugRequest::Attach(AttachRequest {
                process_id: Some(42),
            }),
            stop_on_entry: None,
        };

        let scenario = convert_debug_config(config).unwrap();
        assert!(
            scenario.config.contains(r#""processId":42"#),
            "attach scenario should include process id"
        );
    }
}
