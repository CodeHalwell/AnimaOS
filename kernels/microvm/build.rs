fn main() {
    let rcgen::CertifiedKey { cert, key_pair } =
        rcgen::generate_simple_self_signed(["localhost".to_string()]).unwrap();
    let cert_der = cert.der();
    let key_der = key_pair.serialize_der();

    let out_dir = std::env::var("OUT_DIR").unwrap();
    std::fs::write(format!("{out_dir}/tls_cert.der"), cert_der.as_ref()).unwrap();
    std::fs::write(format!("{out_dir}/tls_key.der"), &key_der).unwrap();

    println!("cargo:rerun-if-changed=build.rs");
}
