//! Swift-specific UniFFI generator: Swift sources, C headers and an
//! XCFramework-compatible modulemap. Used by `ios/build-rust.sh`.
fn main() {
    uniffi::uniffi_bindgen_swift()
}
