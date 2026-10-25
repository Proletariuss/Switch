#![forbid(unsafe_code)]
// Compiles the exact shipping validation/argument modules; no service modules.
#[path = "../../../rust/crates/proletariusd/src/acceptance_capsule.rs"]
mod acceptance_capsule;
#[path = "../../../rust/crates/proletariusd/src/daemon_args.rs"]
mod daemon_args;
fn main() -> std::process::ExitCode {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.first().is_some_and(|a| a == "--kernel-validate") {
        let Some(path) = args.get(1).and_then(|a| a.to_str()) else {
            return 78.into();
        };
        return match acceptance_capsule::load(path) {
            Ok(_) => {
                eprintln!("VALIDATED_CONTEXT_RUNTIME_NO_GO");
                78.into()
            }
            Err(_) => 78.into(),
        };
    }
    if acceptance_capsule::pre_entry(&args).is_err() {
        return 78.into();
    }
    // The same pre_entry call precedes required_app_token/data_root in main/run.
    println!("SERVICE_INITIALIZATION_SENTINEL");
    0.into()
}
