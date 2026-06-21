use clap::Parser;
use std::process::Command;

/// Simple program to greet a person
#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    /// The path to the dir we want to get the git diff from
    path: String,
}

fn print_git_diff(path: String) {
    let output = Command::new("git")
        .arg("diff")
        .arg("--unified=0")
        .arg("--no-color")
        .current_dir(path)
        .output()
        .expect("Failed to execute command");

    if output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        println!("Command Output:\n{}", stdout);
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        eprintln!("Command failed with errors:\n{}", stderr);
    }
}
fn main() {
    let args = Args::parse();

    let input_path: String = args.path;
    print_git_diff(input_path);
}
