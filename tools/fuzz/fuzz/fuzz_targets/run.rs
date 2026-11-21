#![no_main]
libfuzzer_sys::fuzz_target!(|data: &[u8]| gbc_fuzz::fuzz_run(data));
