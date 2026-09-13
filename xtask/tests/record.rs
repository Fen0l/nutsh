//! The recorder's masking rules. Every case here is one a recording actually produced:
//! a fixture is the one artefact of a lab session that ends up in the repository, so a rule
//! that misses a field ships the real value.

use serde_json::{Value, json};
use xtask::record::{leaks, redact};

/// The value a key ends up with, by dotted path, for readable assertions.
fn at<'a>(v: &'a Value, path: &str) -> &'a Value {
    let mut cur = v;
    for step in path.split('.') {
        cur = match step.parse::<usize>() {
            Ok(i) => &cur[i],
            Err(_) => &cur[step],
        };
    }
    cur
}

fn s<'a>(v: &'a Value, path: &str) -> &'a str {
    at(v, path)
        .as_str()
        .unwrap_or_else(|| panic!("{path} is not a string: {}", at(v, path)))
}

#[test]
fn a_serial_is_hashed_wherever_it_is_named() {
    // `serialId` is the v4 spelling, and an ends-with rule saw only `nodeSerial`. The
    // values are invented: a test that pins a real serial leaks it exactly as a fixture does.
    let out = redact(json!({
        "nodeSerial": "EXAMPLENODE0001",
        "blockSerial": "EXAMPLEBLOCK001",
        "serialId": "EXAMPLEDRIVE001",
        "disk": [{ "serialId": "EXAMPLEDRIVE0000002" }],
        "disks": [{ "backingInfo": { "serialId": "NFS_1_0_000_00000000" } }],
    }));
    for (path, key) in [
        ("nodeSerial", "nodeSerial"),
        ("blockSerial", "blockSerial"),
        ("serialId", "serialId"),
        ("disk.0.serialId", "serialId"),
        ("disks.0.backingInfo.serialId", "serialId"),
    ] {
        let got = s(&out, path);
        assert!(
            got.starts_with(&format!("{key}-")) && got.len() == key.len() + 9,
            "{path} kept {got}"
        );
    }
}

#[test]
fn a_serial_goes_with_the_path_that_repeats_it() {
    // `Host.disk[]` spells the drive serial twice: once as `serialId`, once inside the
    // mount path. Hashing one and leaving the other committed the serial anyway.
    let out = redact(json!({
        "disk": [{
            "serialId": "EXAMPLEDRIVE001",
            "mountPath": "/home/nutanix/data/stargate-storage/disks/EXAMPLEDRIVE001",
        }],
    }));
    let hashed = s(&out, "disk.0.serialId");
    assert_eq!(
        s(&out, "disk.0.mountPath"),
        format!("/home/nutanix/data/stargate-storage/disks/{hashed}"),
        "the path keeps its shape and loses the serial",
    );
}

#[test]
fn an_alert_parameter_and_the_title_rendered_from_it_go_together() {
    // The bag is untyped: its key promises nothing and a recorded one holds gateway and
    // load-balancer names. The title is that bag rendered, so it carries them too.
    let out = redact(json!({
        "title": "Load Balancer Session example-lb-session Targets Are Unhealthy",
        "message": "Not all targets of load balancer session: {name} are healthy.",
        "parameters": [
            { "paramName": "load_balancer_session_name",
              "paramValue": { "$objectType": "monitoring.v4.common.StringValue",
                              "stringValue": "example-lb-session" } },
            { "paramValue": { "stringValue": "b53947bf-b6c7-459b-bed6-2c5a9317a21a" } },
            { "paramValue": { "intValue": 2 } },
        ],
    }));
    let hashed = s(&out, "parameters.0.paramValue.stringValue");
    assert!(hashed.starts_with("stringValue-"), "the bag kept {hashed}");
    assert_eq!(
        s(&out, "title"),
        format!("Load Balancer Session {hashed} Targets Are Unhealthy"),
    );
    assert_eq!(
        s(&out, "parameters.1.paramValue.stringValue"),
        "b53947bf-b6c7-459b-bed6-2c5a9317a21a",
        "a uuid in the bag is an extId: references stay resolvable",
    );
    assert_eq!(at(&out, "parameters.2.paramValue.intValue"), &json!(2));
    assert_eq!(
        s(&out, "parameters.0.paramValue.$objectType"),
        "monitoring.v4.common.StringValue",
        "a $-prefixed key carries schema identity",
    );
    assert!(
        s(&out, "message").contains("{name}"),
        "a template is not prose"
    );
}

#[test]
fn a_search_domain_list_is_hashed_like_a_single_one() {
    // `searchDomains`, `nameServers`, `learnedIpAddresses`: the API pluralises, and a
    // suffix rule that knows only the singular let the customer's DNS domains through.
    let out = redact(json!({
        "dhcpOptions": {
            "searchDomains": ["lab.example.com", "lab.example.test"],
            "domainNameServers": [{ "ipv4": { "value": "10.0.0.9" } }],
        },
    }));
    for i in 0..2 {
        let got = s(&out, &format!("dhcpOptions.searchDomains.{i}"));
        assert!(got.starts_with("searchDomains-"), "kept {got}");
    }
    assert_eq!(
        s(&out, "dhcpOptions.domainNameServers.0.ipv4.value"),
        "198.51.100.212",
        "an address under an identity key is masked, not hashed: the shape survives",
    );
}

#[test]
fn a_name_inside_a_wrapper_is_still_a_name() {
    let out = redact(json!({ "ownedBy": { "name": "admin", "extId": "0-0" } }));
    assert!(s(&out, "ownedBy.name").starts_with("name-"));
    assert_eq!(s(&out, "ownedBy.extId"), "0-0");
}

#[test]
fn masking_is_idempotent() {
    // What makes `cargo xtask scrub` a small diff: a tightened rule rewrites what it newly
    // catches and nothing else. Every masked shape has to survive a second pass unchanged.
    let once = redact(json!({
        "hostName": "pc.lab.example",
        "serialId": "EXAMPLEDRIVE001",
        "mountPath": "/home/nutanix/data/stargate-storage/disks/EXAMPLEDRIVE001",
        "title": "Disk space usage high on 10.0.0.8 and 10.0.0.9",
        "ipAddress": { "value": "10.0.0.8" },
        "macAddress": "50:6b:8d:6e:2f:9c",
        "guestCustomization": { "config": { "sysprep": "…" } },
        "password": "hunter2",
        "emailId": "someone@corp.example.com",
        "parameters": [{ "paramValue": { "stringValue": "example-bgp-gw" } }],
        "version": "6.5.3.5",
    }));
    let twice = redact(once.clone());
    assert_eq!(once, twice, "a second pass must be a no-op");
    assert_eq!(
        s(&once, "version"),
        "6.5.3.5",
        "a version is not an address"
    );
    assert_eq!(s(&once, "macAddress"), "00:00:5e:00:53:01");
    assert!(s(&once, "ipAddress.value").starts_with("203.0.113."));
    assert_eq!(s(&once, "password"), "[redacted]");
    assert_eq!(s(&once, "guestCustomization"), "[redacted]");
    assert!(leaks(&once, "").is_empty(), "{:?}", leaks(&once, ""));
}

#[test]
fn a_short_value_is_hashed_but_never_substituted() {
    // `/home` and `7.6` are hashed in the bag like everything else, but substituting them
    // into every string that happens to contain them would corrupt paths and timestamps.
    let out = redact(json!({
        "parameters": [
            { "paramValue": { "stringValue": "/home" } },
            { "paramValue": { "stringValue": "7.6" } },
        ],
        "title": "Disk space usage high for /home",
        "completedTime": "2026-09-05T15:20:07.612345Z",
    }));
    assert_eq!(s(&out, "title"), "Disk space usage high for /home");
    assert_eq!(s(&out, "completedTime"), "2026-09-05T15:20:07.612345Z");
    assert!(s(&out, "parameters.0.paramValue.stringValue").starts_with("stringValue-"));
}

#[test]
fn a_serial_named_key_that_holds_no_string_keeps_its_shape() {
    // The contains rule widens what `serial` matches; nothing but a string is hashed.
    let out = redact(json!({
        "serialPorts": [],
        "isCommunicationActiveOverSerialPort": true,
        "serialPortCount": 2,
    }));
    assert_eq!(
        out,
        json!({
            "serialPorts": [],
            "isCommunicationActiveOverSerialPort": true,
            "serialPortCount": 2,
        }),
    );
}

#[test]
fn every_secret_shape_is_still_redacted() {
    let out = redact(json!({
        "password": "hunter2",
        "passphrase": "correct horse",
        "apiKey": "AKIAEXAMPLE",
        "secretAccessKey": "wJalrEXAMPLEKEY",
        "credentials": [{ "username": "svc", "password": "hunter2" }, "opaque-blob"],
        "guestCustomization": { "config": { "sysprep": { "unattendXml": "<xml/>" } } },
        "cloudInitScript": "I2Nsb3VkLWNvbmZpZwo=",
        "userDataScript": "#!/bin/sh",
        "sysprepScript": "<unattend/>",
        "customizationScript": "#!/bin/sh",
        "config": { "authorizedPublicKeyList": [
            { "name": "ops", "key": "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIExample ops@lab" },
        ] },
    }));
    for path in [
        "password",
        "passphrase",
        "apiKey",
        "secretAccessKey",
        "guestCustomization",
        "cloudInitScript",
        "userDataScript",
        "sysprepScript",
        "customizationScript",
        "config.authorizedPublicKeyList.0.key",
    ] {
        assert_eq!(s(&out, path), "[redacted]", "{path} survived");
    }
    // A list under a secret key keeps its shape: bare strings are the secret, objects are
    // walked so each of their own keys is judged.
    assert_eq!(s(&out, "credentials.1"), "[redacted]");
    assert_eq!(s(&out, "credentials.0.password"), "[redacted]");
    assert!(s(&out, "credentials.0.username").starts_with("username-"));
}

#[test]
fn a_marker_never_matches_inside_an_unrelated_word() {
    // `description` contains `script`. As a contains marker it blanked every description in
    // the corpus - data loss, not masking - and the other four names describe a secret
    // rather than holding one.
    let out = redact(json!({
        "operationDescription": "Deleting recovery point for VM name-1a2b3c4d",
        "hostDescription": "rack 4, top of unit",
        "entityDescription": "nightly backup",
        "stepDescription": "quiesce",
        "versionDescription": "el9-release",
        "subscriptionId": "sub-0001",
        "shouldEnableScriptExec": true,
        "claimTokenExtId": "b53947bf-b6c7-459b-bed6-2c5a9317a21a",
        "accessKeyName": "backup-svc",
        "apiCredentialStatus": "ACTIVE",
        "credentialIssuer": "prism-central",
        "authorizationPolicyType": "PREDEFINED_READ_ONLY",
    }));
    for path in [
        "operationDescription",
        "hostDescription",
        "entityDescription",
        "stepDescription",
        "versionDescription",
        "subscriptionId",
        "claimTokenExtId",
        "apiCredentialStatus",
        "authorizationPolicyType",
    ] {
        assert_ne!(s(&out, path), "[redacted]", "{path} was blanked");
    }
    assert_eq!(
        s(&out, "operationDescription"),
        "Deleting recovery point for VM name-1a2b3c4d",
        "task prose stays readable; a name inside it is repaired by the substitution pass",
    );
    assert_eq!(at(&out, "shouldEnableScriptExec"), &json!(true));
    // Kept, but still judged: a name is hashed, an issuer is prose.
    assert!(s(&out, "accessKeyName").starts_with("accessKeyName-"));
    assert_eq!(s(&out, "credentialIssuer"), "prism-central");
}

#[test]
fn a_uuid_with_a_node_suffix_is_not_an_ipv6_address() {
    // `Disk.serviceVMId` is `<cluster uuid>::<node id>`, and its tail `c1da::9` parses as an
    // IPv6 address: the scan refused every disk fixture the recording recorded until it learned
    // that a match welded to the hex around it is a fragment of an identifier.
    let disk = json!([{
        "extId": "0323d129-73e1-42ad-b2af-a8d5928ac2c2",
        "serviceVMId": "000623f4-7174-ae2a-0000-00000001c1da::9",
    }]);
    assert!(leaks(&disk, "").is_empty(), "{:?}", leaks(&disk, ""));
    // An address written on its own is still refused.
    let real = json!({ "ipv6": "fd00::5" });
    assert_eq!(leaks(&real, ""), vec!["ipv6: ipv6 fd00::5".to_string()]);
}

#[test]
fn a_category_names_whoever_made_it() {
    // `VmRecoveryPoint.vmCategories` is a list of `key/value` strings, and a recorded one
    // holds the owner's own name on both sides of the slash. Both halves are hashed: a category key
    // names its owner as readily as its value does.
    let out = redact(json!({
        "vmCategories": ["Owner/Production", "KubernetesClusterName/mgmt-prod"],
        "categoryExtIds": ["b33b5038-33ed-36cc-a7de-f0b6e5e861d1"],
    }));
    for i in 0..2 {
        assert!(
            s(&out, &format!("vmCategories.{i}")).starts_with("vmCategories-"),
            "{}",
            s(&out, &format!("vmCategories.{i}")),
        );
    }
    // A category named by its uuid is a reference, and references stay resolvable.
    assert_eq!(
        s(&out, "categoryExtIds.0"),
        "b33b5038-33ed-36cc-a7de-f0b6e5e861d1"
    );
}
