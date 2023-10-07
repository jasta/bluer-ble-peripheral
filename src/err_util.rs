pub fn generic_err(debug_message: &str) -> bluer::Error {
    bluer::Error::from(std::io::Error::new(
        std::io::ErrorKind::Other,
        debug_message,
    ))
}
