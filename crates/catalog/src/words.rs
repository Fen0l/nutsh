//! Words: the acronym table, and the two ways a machine word is written for a person.
//!
//! It lives in the catalog because both sides need it. `xtask` prettifies display names and
//! column headers at generation time; `nutsh_core::cell` sentence-cases enum values at render
//! time. One table, so a header and the value under it cannot disagree about how `Vm` is
//! spelled.

/// Machine casing → the casing a person writes, matched on one capitalised word.
pub const ACRONYMS: &[(&str, &str)] = &[
    ("Vm", "VM"),
    ("Vms", "VMs"),
    ("Ip", "IP"),
    ("Ips", "IPs"),
    ("Vpc", "VPC"),
    ("Vpcs", "VPCs"),
    ("Vpn", "VPN"),
    ("Vpns", "VPNs"),
    ("Saml", "SAML"),
    ("Iscsi", "iSCSI"),
    ("Nic", "NIC"),
    ("Nics", "NICs"),
    ("Gpu", "GPU"),
    ("Gpus", "GPUs"),
    ("Cpu", "CPU"),
    ("Cpus", "CPUs"),
    ("Ssl", "SSL"),
    ("Ssh", "SSH"),
    ("Ldap", "LDAP"),
    ("Dns", "DNS"),
    ("Ntp", "NTP"),
    ("Smtp", "SMTP"),
    ("Snmp", "SNMP"),
    ("Ncc", "NCC"),
    ("Lcm", "LCM"),
    ("Pcie", "PCIe"),
    ("Bgp", "BGP"),
    ("Ospf", "OSPF"),
    ("Nfs", "NFS"),
    ("Smb", "SMB"),
    ("Cvm", "CVM"),
    ("Cvms", "CVMs"),
    ("Ahv", "AHV"),
    ("Esxi", "ESXi"),
    ("Api", "API"),
    ("Id", "ID"),
    ("Ids", "IDs"),
    ("Uuid", "UUID"),
    ("Mac", "MAC"),
    ("Vlan", "VLAN"),
    ("Vlans", "VLANs"),
    ("Nat", "NAT"),
    ("Bmc", "BMC"),
    ("Ipmi", "IPMI"),
    ("Tls", "TLS"),
    ("Oidc", "OIDC"),
    ("Iam", "IAM"),
    ("Pc", "PC"),
    ("Pe", "PE"),
    ("Ipv4", "IPv4"),
    ("Ipv6", "IPv6"),
    ("Kms", "KMS"),
    ("Vtep", "VTEP"),
    // Six for enum values this phase renders: a protection domain, an ISO image, an OVA, a
    // multiple spanning tree, the storage tier `SSD_PCIE` - whose second word `Pcie` above
    // already spells, and whose first is an acronym like any other - and a disk bus. `Iscsi`
    // above is a different word and keeps its own spelling.
    ("Pd", "PD"),
    ("Iso", "ISO"),
    ("Ova", "OVA"),
    ("Mst", "MST"),
    ("Ssd", "SSD"),
    ("Scsi", "SCSI"),
    // Five more for the rest of the words those same enums are built from: `SSD_PCIE` is one
    // of five `StorageTier` values and the other four are `SSD_SATA`, `DAS_SATA`,
    // `SSD_MEM_NVME` and `CLOUD`, and a NIC profile's capability is `SRIOV` or `DP_OFFLOAD`.
    // Without them `sentence_case` lowercases the token and the tier reads `SSD sata`.
    ("Sata", "SATA"),
    ("Nvme", "NVMe"),
    ("Das", "DAS"),
    ("Sriov", "SR-IOV"),
    ("Dp", "DP"),
];

/// Restore acronym casing word by word: `Vm Anti Affinity Policies` → `VM Anti Affinity
/// Policies`. What display names and column headers go through.
pub fn prettify(display: &str) -> String {
    display
        .split(' ')
        .map(|w| {
            ACRONYMS
                .iter()
                .find(|(from, _)| *from == w)
                .map(|(_, to)| *to)
                .unwrap_or(w)
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// `DISK_IMAGE` → `Disk image`; `VM_ANTI_AFFINITY` → `VM anti affinity`; `AHV` → `AHV`;
/// `IPV4` → `IPv4`.
///
/// Splits on `_`, maps each token through [`ACRONYMS`] keyed on its capitalised form, and
/// otherwise capitalises the first token and lowercases the rest. An acronym keeps its casing
/// wherever it falls, because `VM anti affinity` reads and `Vm anti affinity` does not.
pub fn sentence_case(word: &str) -> String {
    let mut out = String::with_capacity(word.len());
    for (i, token) in word.split('_').filter(|t| !t.is_empty()).enumerate() {
        if i > 0 {
            out.push(' ');
        }
        // Keyed case-insensitively on the token itself: every key in the table is one
        // capitalised ASCII word, so the capitalised form is needed only to emit a miss, and
        // this runs for every `Enum` cell of every frame.
        match ACRONYMS
            .iter()
            .find(|(from, _)| from.eq_ignore_ascii_case(token))
        {
            Some((_, to)) => out.push_str(to),
            None if i == 0 => out.push_str(&capitalize(&token.to_ascii_lowercase())),
            None => out.extend(token.chars().map(|c| c.to_ascii_lowercase())),
        }
    }
    out
}

/// `vm` → `Vm`. Shared by [`sentence_case`] and the generator's display-name derivation.
pub fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The four cases the spec names, plus the shape every other enum value takes.
    #[test]
    fn sentence_case_keeps_the_acronyms_and_lowercases_the_rest() {
        for (word, want) in [
            ("DISK_IMAGE", "Disk image"),
            ("PLANNED_FAILOVER", "Planned failover"),
            ("AHV", "AHV"),
            ("VM_ANTI_AFFINITY", "VM anti affinity"),
            ("IPV4", "IPv4"),
            ("ISO", "ISO"),
            ("OVA", "OVA"),
            ("PD", "PD"),
            ("MST", "MST"),
            ("ON", "On"),
            ("IN_MAINTENANCE", "In maintenance"),
            ("SUCCEEDED", "Succeeded"),
            ("VM_HOST_AFFINITY", "VM host affinity"),
            // The five `StorageTier` values, whole: four of them were half-cased until
            // `Sata`, `Nvme` and `Das` joined the table.
            ("SSD_PCIE", "SSD PCIe"),
            ("SSD_SATA", "SSD SATA"),
            ("DAS_SATA", "DAS SATA"),
            ("SSD_MEM_NVME", "SSD mem NVMe"),
            ("CLOUD", "Cloud"),
            // A NIC profile's two capabilities.
            ("SRIOV", "SR-IOV"),
            ("DP_OFFLOAD", "DP offload"),
            ("", ""),
        ] {
            assert_eq!(sentence_case(word), want, "{word}");
        }
    }

    /// `prettify` moved with the table and still answers exactly as it did in `xtask`.
    #[test]
    fn prettify_restores_acronym_casing_word_by_word() {
        assert_eq!(
            prettify("Vm Anti Affinity Policies"),
            "VM Anti Affinity Policies"
        );
        assert_eq!(
            prettify("Iscsi Client Attachments"),
            "iSCSI Client Attachments"
        );
        assert_eq!(prettify("Vms"), "VMs");
        assert_eq!(prettify("Widgets"), "Widgets");
    }

    /// The eleven pairs this phase adds are in the table, and the table is still sorted by
    /// nothing in particular - it is searched linearly and its order is its history.
    ///
    /// `Ssd` and `Scsi` are the two the plan did not name: without a case here the only thing
    /// holding `Scsi` down is a `.snap` a later task re-records, which would absorb its removal
    /// rather than catch it.
    #[test]
    fn the_new_acronyms_are_in_the_table() {
        for (from, to) in [
            ("Pd", "PD"),
            ("Iso", "ISO"),
            ("Ova", "OVA"),
            ("Mst", "MST"),
            ("Ssd", "SSD"),
            ("Scsi", "SCSI"),
            ("Sata", "SATA"),
            ("Nvme", "NVMe"),
            ("Das", "DAS"),
            ("Sriov", "SR-IOV"),
            ("Dp", "DP"),
        ] {
            assert!(ACRONYMS.contains(&(from, to)), "{from} -> {to} is missing");
        }
    }
}
