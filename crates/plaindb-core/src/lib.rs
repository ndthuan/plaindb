pub fn format_version() -> u32 {
    1
}

#[cfg(test)]
mod tests {
    use super::format_version;

    #[test]
    fn format_version_is_one() {
        assert_eq!(format_version(), 1);
    }
}
