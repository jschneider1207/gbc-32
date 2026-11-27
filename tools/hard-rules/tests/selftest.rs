//! Runs the shared checker's own tests in this package.

#[test]
fn allows_test_modules_comments_strings_and_core_alloc() {
    hard_rules::selftest::allows_test_modules_comments_strings_and_core_alloc();
}

#[test]
fn catches_extern_crate_however_written() {
    hard_rules::selftest::catches_extern_crate_however_written();
}

#[test]
fn catches_path_and_include() {
    hard_rules::selftest::catches_path_and_include();
}

#[test]
fn catches_std_and_alloc_paths() {
    hard_rules::selftest::catches_std_and_alloc_paths();
}

#[test]
fn catches_unsafe_anywhere() {
    hard_rules::selftest::catches_unsafe_anywhere();
}

#[test]
fn crate_attributes_are_found() {
    hard_rules::selftest::crate_attributes_are_found();
}

#[test]
fn dependency_tables_are_found() {
    hard_rules::selftest::dependency_tables_are_found();
}

#[test]
fn escaped_quote_char_literals_lex_as_one_token() {
    hard_rules::selftest::escaped_quote_char_literals_lex_as_one_token();
}

#[test]
fn leading_inner_attributes_only() {
    hard_rules::selftest::leading_inner_attributes_only();
}

#[test]
fn manifest_escape_hatches_are_found() {
    hard_rules::selftest::manifest_escape_hatches_are_found();
}
