pub fn protocol_version() -> u32 {
    1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_is_one() {
        assert_eq!(protocol_version(), 1);
    }
}
