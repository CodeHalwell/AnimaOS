fn main() {
    let cert = rcgen::generate_simple_self_signed(["localhost".to_string()]).unwrap();
    let cert_der = cert.serialize_der().unwrap();
    let key_der = cert.get_key_pair().serialize_der();

    let out_dir = std::env::var("OUT_DIR").unwrap();
    std::fs::write(format!("{out_dir}/tls_cert.der"), &cert_der).unwrap();
    std::fs::write(format!("{out_dir}/tls_key.der"), &key_der).unwrap();

    println!("cargo:rerun-if-changed=build.rs");
}
