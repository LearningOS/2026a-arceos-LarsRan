#![cfg_attr(feature = "axstd", no_std)]
#![cfg_attr(feature = "axstd", no_main)]

#[cfg(feature = "axstd")]
use axstd::println;

/// ANSI 转义序列：绿色前景色（写法与 axlog 内部的 `with_color!` 保持一致）
const GREEN: &str = "\u{1B}[32m";
/// ANSI 转义序列：重置所有文字属性
const RESET: &str = "\u{1B}[m";

#[cfg_attr(feature = "axstd", no_mangle)]
fn main() {
    println!("{}[WithColor]: Hello, Arceos!{}", GREEN, RESET);
}
