fn main() {
    println!("cargo::rustc-check-cfg=cfg(no_lock_order)");
    println!("cargo::rustc-cfg=no_lock_order");
}
