fn main() {
    let mut static_rules = check_rust::rules();
    static_rules.extend(check_deps::rules());
    let mut factories = check_core::kinds::builtin_factories();
    factories.extend(check_deps::factories());
    factories.extend(check_git::factories());
    std::process::exit(check_core::cli::run(check_core::cli::Frontend {
        static_rules,
        factories,
    }));
}
