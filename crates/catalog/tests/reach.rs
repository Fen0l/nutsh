use nutsh_catalog::{KINDS, Reach, children, fill_placeholders, kind, parent_chain, reach};

#[test]
fn fill_placeholders_goes_left_to_right() {
    assert_eq!(
        fill_placeholders("/vmm/v4.3/ahv/config/vms/{vmExtId}/disks", &["vm1"]),
        Some("/vmm/v4.3/ahv/config/vms/vm1/disks".to_string())
    );
    assert_eq!(
        fill_placeholders(
            "/c/v4.3/config/clusters/{a}/hosts/{b}/host-nics",
            &["c1", "h1"]
        ),
        Some("/c/v4.3/config/clusters/c1/hosts/h1/host-nics".to_string())
    );
    assert_eq!(
        fill_placeholders("/vmm/v4.3/ahv/config/vms", &[]),
        Some("/vmm/v4.3/ahv/config/vms".into())
    );
    assert_eq!(fill_placeholders("/x/{a}", &[]), None, "too few values");
    assert_eq!(
        fill_placeholders("/x/{a}", &["1", "2"]),
        None,
        "too many values"
    );
}

#[test]
fn children_are_sorted_by_display_and_parents_link_back() {
    let vm = kind("vmm.ahv.config.Vm").unwrap();
    let kids = children(vm.id);
    // Six, not seven: `vms/{extId}/guest-tools` answers with one object rather than a
    // collection, so the generator no longer builds a list kind on it.
    assert_eq!(
        kids.len(),
        6,
        "{:?}",
        kids.iter().map(|k| k.id).collect::<Vec<_>>()
    );
    let displays: Vec<&str> = kids.iter().map(|k| k.display).collect();
    let mut sorted = displays.clone();
    sorted.sort_unstable();
    assert_eq!(displays, sorted);
    assert!(kids.iter().all(|k| k.parent == Some(vm.id)));
    assert!(children("vmm.ahv.config.Disk").is_empty());
}

#[test]
fn parent_chain_is_root_first() {
    let nic = kind("clustermgmt.config.HostNic~host-nics").unwrap();
    let chain: Vec<&str> = parent_chain(nic).iter().map(|k| k.id).collect();
    assert_eq!(
        chain,
        [
            "clustermgmt.config.Cluster",
            "clustermgmt.config.Host~hosts"
        ]
    );
    assert!(parent_chain(kind("vmm.ahv.config.Vm").unwrap()).is_empty());
}

#[test]
fn reach_classifies_every_kind_as_the_spec_counted() {
    let mut direct = 0;
    let mut from_parent = 0;
    let mut greyed = 0;
    for k in KINDS {
        match reach(k) {
            Reach::Direct => direct += 1,
            Reach::FromParent(_) => from_parent += 1,
            Reach::NeedsParameter => greyed += 1,
        }
    }
    // Thirty fewer than the 262 of before `lists_a_collection`: every kind the generator had
    // built on a GET that answers with one object, or with a file, is gone.
    assert_eq!((direct, from_parent, greyed), (135, 83, 14));
    assert_eq!(direct + from_parent + greyed, KINDS.len());
    assert_eq!(reach(kind("vmm.ahv.config.Vm").unwrap()), Reach::Direct);
    assert_eq!(
        reach(kind("vmm.ahv.config.Disk").unwrap()),
        Reach::FromParent(kind("vmm.ahv.config.Vm").unwrap())
    );
    assert_eq!(
        reach(kind("aiops.config.EntityDescriptor").unwrap()),
        Reach::NeedsParameter
    );
    assert_eq!(
        reach(kind("files.config.VdiUserSession").unwrap()),
        Reach::NeedsParameter
    );
}

#[test]
fn from_parent_kinds_never_hang_off_a_greyed_ancestor() {
    for k in KINDS {
        if matches!(reach(k), Reach::FromParent(_)) {
            for ancestor in parent_chain(k) {
                assert!(
                    matches!(reach(ancestor), Reach::Direct | Reach::FromParent(_)),
                    "{} has ancestor {} which is {:?}",
                    k.id,
                    ancestor.id,
                    reach(ancestor)
                );
            }
        }
    }
}

#[test]
fn reach_reason_texts() {
    assert_eq!(
        reach(kind("vmm.ahv.config.Disk").unwrap()).reason(),
        Some("open from Virtual Machines".to_string())
    );
    assert_eq!(
        reach(kind("aiops.config.EntityDescriptor").unwrap()).reason(),
        Some("needs a parameter".to_string())
    );
    assert_eq!(reach(kind("vmm.ahv.config.Vm").unwrap()).reason(), None);
}
