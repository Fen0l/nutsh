use nutsh_catalog::{KINDS, NAMESPACES, kind, namespace, version_key};

#[test]
fn version_key_orders_alpha_before_beta_before_ga() {
    assert_eq!(version_key("v4.3"), [4, 3, 2, 0]);
    assert_eq!(version_key("v4.3.b1"), [4, 3, 1, 1]);
    assert_eq!(version_key("v4.0.a3"), [4, 0, 0, 3]);
    assert!(version_key("v4.0.a3") < version_key("v4.0.b1"));
    assert!(version_key("v4.0.b1") < version_key("v4.0"));
    assert!(version_key("v4.0") < version_key("v4.1"));
    assert!(version_key("v4.10") > version_key("v4.9"));

    // Degenerate input sorts low or as GA instead of panicking.
    assert_eq!(version_key("v4.0."), [4, 0, 2, 0]);
    assert_eq!(version_key("v4"), [4, 0, 2, 0]);
    assert_eq!(version_key(""), [0, 0, 2, 0]);
}

#[test]
fn namespace_versions_start_at_the_catalog_version_and_descend() {
    for n in NAMESPACES {
        assert_eq!(n.versions.first(), Some(&n.version), "{}", n.name);
        for pair in n.versions.windows(2) {
            assert!(
                version_key(pair[0]) > version_key(pair[1]),
                "{} {:?}",
                n.name,
                n.versions
            );
        }
    }
}

#[test]
fn every_since_is_a_known_version_no_newer_than_the_kind() {
    for k in KINDS {
        let ns = namespace(k.namespace).unwrap_or_else(|| panic!("{} has no namespace", k.id));
        assert!(
            ns.versions.contains(&k.since),
            "{} since {} not in {:?}",
            k.id,
            k.since,
            ns.versions
        );
        assert!(
            version_key(k.since) <= version_key(k.version),
            "{} since {} newer than {}",
            k.id,
            k.since,
            k.version
        );
    }
}

#[test]
fn served_at_compares_since_with_the_pinned_version() {
    let vm = kind("vmm.ahv.config.Vm").unwrap();
    assert!(vm.served_at(vm.version));
    assert!(vm.served_at("v9.9"));
    assert!(!vm.served_at("v3.0"));
}

#[test]
fn real_since_values_follow_the_specs() {
    // `vms` has been listed since vmm v4.0; `vm-profiles` first appears in the v4.3 spec.
    assert_eq!(kind("vmm.ahv.config.Vm").unwrap().since, "v4.0");
    assert_eq!(kind("vmm.ahv.config.VmProfile").unwrap().since, "v4.3");
    let vmm = namespace("vmm").unwrap();
    assert_eq!(vmm.versions, &["v4.3", "v4.2", "v4.1", "v4.0"]);
    let storage = namespace("storage").unwrap();
    assert_eq!(
        storage.versions,
        &[storage.version],
        "preview-only namespaces list themselves"
    );
}

#[test]
fn with_version_swaps_only_the_version_segment() {
    use nutsh_catalog::with_version;
    assert_eq!(
        with_version("/vmm/v4.3/ahv/config/vms", "v4.1"),
        "/vmm/v4.1/ahv/config/vms"
    );
    assert_eq!(
        with_version(
            "/clustermgmt/v4.3/config/clusters/{clusterExtId}/hosts",
            "{v}"
        ),
        "/clustermgmt/{v}/config/clusters/{clusterExtId}/hosts"
    );
    assert_eq!(with_version("/vmm/v4.3", "v4.1"), "/vmm/v4.1");
    assert_eq!(
        with_version("/vmm", "v4.1"),
        "/vmm",
        "too short to have a version"
    );
    assert_eq!(
        with_version("vmm/v4.3/x", "v4.1"),
        "vmm/v4.3/x",
        "no leading slash"
    );
}

#[test]
fn version_in_reads_the_version_segment() {
    use nutsh_catalog::version_in;
    assert_eq!(version_in("/vmm/v4.3/ahv/config/vms"), Some("v4.3"));
    assert_eq!(version_in("/vmm/v4.3"), Some("v4.3"));
    assert_eq!(version_in("/vmm"), None);
    assert_eq!(version_in("/vmm/"), None);
    assert_eq!(version_in("vmm/v4.3/x"), None);
}
