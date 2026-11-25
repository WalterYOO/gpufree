//! gpufree — check GPU availability across SSH hosts.
//!
//! 设计阶段骨架：按 docs/DESIGN.md 从 M1 起实现。

// 骨架阶段尚未用到全部依赖；M1 完成、依赖真正接入后删除此 allow。
#![allow(unused_crate_dependencies)]

mod cli;
mod config;
mod doctor;
mod model;
mod output;
mod probe;
mod tui;

use clap::Parser;

fn main() {
    let _cli = cli::Cli::parse();
    eprintln!("gpufree: 尚未实现，设计稿见 docs/DESIGN.md");
    std::process::exit(2);
}
