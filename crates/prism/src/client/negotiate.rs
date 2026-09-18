//! Version negotiation and what this Prism Central serves: pins, probes, repairs, availability.

use super::*;

pub(super) const PROBE_CANDIDATES: usize = 3;

/// The shortest gap between two lazy repairs of one namespace, so a failing service cannot be
/// turned into a re-negotiation storm.
pub(super) const REPAIR_INTERVAL: Duration = Duration::from_secs(60);

/// The detail for a namespace that is down at a version: the probed path, then the error.
/// An `Api` error whose body carried no message already names the path (`HTTP 503: /x`), so
/// it is not prefixed a second time.
pub(super) fn down_detail(path: &str, e: &PrismError) -> String {
    let text = e.to_string();
    if text.contains(path) {
        text
    } else {
        format!("{path}: {text}")
    }
}

/// The kinds worth one `$limit=1` list to learn whether `ns` serves `version`: top-level,
/// pageable, parameter-free, known at that version, shortest paths first. More than one,
/// because a served namespace can still 404 one kind.
pub(super) fn probe_candidates(ns: &Namespace, version: &str) -> Vec<&'static Kind> {
    let mut candidates: Vec<&'static Kind> = KINDS
        .iter()
        .filter(|k| {
            k.namespace == ns.name
                && k.is_top_level()
                && k.list_params.limit
                && !k.list_params.required
                && k.served_at(version)
        })
        .collect();
    candidates.sort_by_key(|k| (k.list_path.len(), k.list_path));
    candidates.truncate(PROBE_CANDIDATES);
    candidates
}

/// Whether a failed list says the pin is wrong: the endpoint is absent or the service is down
/// at this version. A 403, a transport failure or a body that will not decode is about the
/// account, the network or the payload, and re-negotiating a namespace fixes none of them.
pub(super) fn worth_repairing(e: &PrismError) -> bool {
    matches!(
        e,
        PrismError::NotFound(_) | PrismError::Api { status: 500.., .. }
    )
}

impl Client {
    /// Route by what the last run negotiated, with zero requests. `statuses` are the restored
    /// positives only: a false "served" costs one 404 that the lazy repair corrects, a false "not
    /// served" hides a pane with no request left to correct it. A namespace with no status is
    /// unknown, not unavailable, and is negotiated lazily on its first failure. A pin the catalog
    /// cannot resolve is dropped alone.
    pub fn adopt_pins(&self, pins: &BTreeMap<String, String>, statuses: Vec<NamespaceStatus>) {
        let mut resolved: HashMap<&'static str, &'static str> = HashMap::new();
        for (name, version) in pins {
            let Some(ns) = NAMESPACES.iter().find(|n| n.name == *name) else {
                continue;
            };
            let Some(v) = ns.versions.iter().find(|v| **v == version.as_str()) else {
                continue;
            };
            resolved.insert(ns.name, v);
        }
        let statuses: Vec<NamespaceStatus> = statuses.into_iter().filter(|s| s.ok).collect();
        // Everything this restore did not *probe*: the namespaces it pinned, and the ones it
        // says nothing about. Both are the lazy repair's to correct, and nothing outside this
        // set may be re-probed by it.
        let adopted: HashSet<&'static str> = NAMESPACES
            .iter()
            .map(|ns| ns.name)
            .filter(|name| resolved.contains_key(name) || !statuses.iter().any(|s| s.name == *name))
            .collect();
        *self
            .pins
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = resolved;
        *self
            .statuses
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = statuses;
        *self
            .adopted
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = adopted;
    }

    /// A list failed under an **adopted** pin in a way that means "not there" or "down at this
    /// version": re-negotiate that namespace alone, at most once per 60 s.
    pub(super) async fn repair_after(&self, namespace: &'static str, e: &PrismError) {
        if !worth_repairing(e) {
            return;
        }
        if !self.is_adopted(namespace) {
            return;
        }
        {
            let mut repaired = self
                .repaired
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let now = std::time::Instant::now();
            if let Some(last) = repaired.get(namespace)
                && now.duration_since(*last) < REPAIR_INTERVAL
            {
                return;
            }
            repaired.insert(namespace, now);
        }
        tracing::debug!(
            namespace,
            "restored pin did not answer; re-negotiating it alone"
        );
        // Boxed because this closes a cycle - a list drives a probe, which is a list - and an
        // async future may not contain itself.
        let _ = Box::pin(self.negotiate_namespace(namespace)).await;
    }

    /// Settles, per namespace, which API version this Prism Central serves: the catalog's
    /// versions newest first, one `$limit=1` list per try, the first non-404 answer pins the
    /// version for every later request through `url`. Runs once per session.
    pub async fn negotiate(&self) -> Result<Vec<NamespaceStatus>, PrismError> {
        // Bad credentials and an unreachable host end negotiation at once; the probes in flight are
        // dropped. Boxed on purpose: opaque probe futures inside a stream combinator are rejected
        // as not `Send` by a `BoxFuture<'static, _>` caller (rust-lang/rust#102211).
        let probes: Vec<BoxFuture<'_, Result<NamespaceStatus, PrismError>>> = NAMESPACES
            .iter()
            .map(|ns| Box::pin(self.probe_namespace(ns)) as BoxFuture<'_, _>)
            .collect();
        let statuses: Vec<NamespaceStatus> = stream::iter(probes)
            .buffered(self.fan_out())
            .try_collect::<Vec<NamespaceStatus>>()
            .await?;
        {
            let mut pins = self
                .pins
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            *pins = statuses
                .iter()
                .filter_map(|s| Some((s.name, s.pinned?)))
                .collect();
        }
        self.statuses
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone_from(&statuses);
        // Nothing here was adopted: every pin above came from a probe.
        self.adopted
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
        Ok(statuses)
    }

    /// Negotiation for one namespace, which is all `ctx login` needs to verify a password.
    pub async fn negotiate_namespace(&self, name: &str) -> Result<NamespaceStatus, PrismError> {
        let ns = NAMESPACES
            .iter()
            .find(|n| n.name == name)
            .ok_or_else(|| PrismError::Catalog(format!("no namespace {name}")))?;
        let status = self.probe_namespace(ns).await?;
        {
            let mut pins = self
                .pins
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            // A namespace that stopped answering must lose its old pin, not keep routing to a
            // version this negotiation could not reach.
            match status.pinned {
                Some(p) => pins.insert(ns.name, p),
                None => pins.remove(ns.name),
            };
        }
        {
            let mut statuses = self
                .statuses
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            statuses.retain(|s| s.name != ns.name);
            statuses.push(status.clone());
        }
        // This pin was probed, so it is no longer one the lazy repair may re-probe.
        self.adopted
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(ns.name);
        Ok(status)
    }

    /// The negotiation result, one row per namespace, empty before `negotiate` runs or a
    /// restore adopts one. An owned `Vec` rather than a slice: the rows live behind an
    /// `RwLock` now, and a borrow cannot outlive the guard. One caller, `src/check.rs`, and
    /// one clone per `--check` run.
    pub fn statuses(&self) -> Vec<NamespaceStatus> {
        self.statuses_guard().clone()
    }

    /// Whether this Prism Central is worth a list request for `kind`: the standing verdict,
    /// and then what the server has already answered about this very endpoint. The predicate
    /// every *volunteered* request passes through - the stats counters, the DR sampler, the
    /// name warm-up, a page's panes - so none of them re-learns a 404 once a cycle.
    pub fn is_served(&self, kind: &Kind) -> bool {
        matches!(self.list_availability(kind), Availability::Served)
    }

    /// The standing verdict on `kind`, with its reason: no request could succeed, because the
    /// namespace answers at no version or the kind is not in the version it pins. Derived from
    /// the pins each time, so nothing here goes stale. A list that answered 404 is
    /// [`Client::list_availability`]'s, not this.
    pub fn availability(&self, kind: &Kind) -> Availability {
        match self.namespace_availability(kind.namespace) {
            Availability::Served => self.version_availability(kind),
            down => down,
        }
    }

    /// The standing verdict, then the 404 this session already paid for: what to say instead of
    /// subscribing. Apart from [`Client::availability`] because a person may still open the table
    /// and `^r` it; what this refuses is a request volunteered on their behalf - a pane, a counter.
    pub fn list_availability(&self, kind: &Kind) -> Availability {
        match self.availability(kind) {
            Availability::Served if self.is_missing(kind) => Availability::ListNotFound,
            verdict => verdict,
        }
    }

    /// Is this namespace reachable: it answered at some version, and that version is pinned. No
    /// status row under a restore is unknown, not unavailable (see `adopt_pins`); with no restore,
    /// nothing has been negotiated.
    pub fn namespace_availability(&self, namespace: &str) -> Availability {
        let known = {
            let guard = self.statuses_guard();
            guard
                .iter()
                .find(|s| s.name == namespace)
                .map(|status| match status.pinned {
                    Some(_) if status.ok => Availability::Served,
                    _ => Availability::NamespaceUnavailable(status.detail.clone()),
                })
        };
        known.unwrap_or_else(|| {
            if self.is_adopted(namespace) {
                // Unknown, not unavailable: see `adopt_pins` for why the two differ.
                Availability::Served
            } else {
                Availability::NamespaceUnavailable("not negotiated".into())
            }
        })
    }

    /// Is this kind in the API this Prism Central serves: the catalog places its first appearance
    /// at or before the pinned version. Read from the pin each time. The trade: a Prism Central
    /// that routes a kind's newer path while probing only at an older one is greyed rather than
    /// asked; `:try` is the way to make the server answer for itself.
    pub fn version_availability(&self, kind: &Kind) -> Availability {
        match self.pins_guard().get(kind.namespace).copied() {
            Some(pinned) if !kind.served_at(pinned) && !self.asked_anyway(kind) => {
                Availability::NotServedAtPin {
                    namespace: kind.namespace,
                    since: kind.since,
                    pinned,
                }
            }
            _ => Availability::Served,
        }
    }

    /// Set the version prediction aside for `kind`: ask this Prism Central once and let the
    /// answer stand. A 404 is remembered by [`Client::mark_missing`] and greys the kind on the
    /// server's own verdict; a 200 is the kind working for the session. Nothing retries, and
    /// nothing calls this on the user's behalf.
    pub fn ask_anyway(&self, kind: &Kind) {
        self.asked
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(kind.id);
    }

    /// Whether [`Client::ask_anyway`] was called for this kind.
    pub fn asked_anyway(&self, kind: &Kind) -> bool {
        self.asked
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains(kind.id)
    }

    /// Whether this Prism Central has already refused a `$select` on `kind`.
    pub fn select_refused(&self, kind: &Kind) -> bool {
        self.no_select
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains(kind.id)
    }

    /// Give up narrowing `kind` for the rest of the session. One way: a 400 is the endpoint
    /// saying the narrowing is wrong, and nothing about it changes while a session is open.
    pub(super) fn refuse_select(&self, kind: &Kind) {
        self.no_select
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(kind.id);
    }

    /// Remember that `kind`'s top-level list answered 404. Called by the scheduler, which is
    /// the only caller that knows which 404 is the endpoint's: see `is_missing_list` there.
    pub fn mark_missing(&self, kind: &Kind) {
        self.missing
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(kind.id);
    }

    /// Forget it: a cycle over `kind` completed, so whatever the 404 was about is over. The
    /// way back for `^r`, and for a service that was still starting when the session opened.
    pub fn forget_missing(&self, kind: &Kind) {
        self.missing
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(kind.id);
    }

    pub(super) fn is_missing(&self, kind: &Kind) -> bool {
        self.missing
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains(kind.id)
    }

    /// The domain manager's `extId` and `config.buildInfo.version` in one read - the request
    /// `connect` already makes - with the error kept: a rejected password or a refused
    /// certificate has to be reportable as itself. `Ok(None)` is an endpoint shaped differently.
    pub async fn pc_identity_result(&self) -> Result<Option<(String, Option<String>)>, PrismError> {
        let Some(kind) = nutsh_catalog::kind("prism.config.DomainManager") else {
            return Ok(None);
        };
        let opts = ListOptions {
            limit: 1,
            ..Default::default()
        };
        let page = self.list_page(kind, 0, &opts).await?;
        let Some(first) = page.entities.first() else {
            return Ok(None);
        };
        let ext_id = first.ext_id.clone();
        if ext_id.is_empty() {
            return Ok(None);
        }
        let version = first
            .raw
            .pointer("/config/buildInfo/version")
            .and_then(Value::as_str)
            .map(str::to_string);
        Ok(Some((ext_id, version)))
    }

    /// [`Client::pc_identity_result`] with every error read as "did not answer", which is what
    /// makes "did not answer" and "is a different Prism Central" distinguishable for a caller
    /// that has already proved the connection some other way.
    pub async fn pc_identity(&self) -> Option<(String, Option<String>)> {
        self.pc_identity_result().await.ok().flatten()
    }

    /// Newest version first; the first served answer pins the namespace. A 404 from every
    /// candidate steps to the next version; a 5xx or an undecodable answer is the service down at
    /// that version, reported and not stepped past. Anything host-wide - auth, connect, TLS, a
    /// surviving 429 - aborts the whole negotiation rather than becoming a session-long verdict.
    pub(super) async fn probe_namespace(
        &self,
        ns: &'static Namespace,
    ) -> Result<NamespaceStatus, PrismError> {
        let opts = ListOptions {
            limit: 1,
            ..Default::default()
        };
        let mut probed_any = false;
        // A version whose candidate list is empty issues no request at all - no listable kind
        // existed at that version yet - and the namespace is still named in the not-served
        // list below. Today every namespace has at least one candidate at every version.
        for &version in ns.versions {
            let mut down: Option<String> = None;
            for k in probe_candidates(ns, version) {
                probed_any = true;
                let path = with_version(k.list_path, version);
                match self.list_page_at(k, 0, &opts, Some(version)).await {
                    Ok(_) => {
                        return Ok(NamespaceStatus::served(
                            ns,
                            version,
                            format!("probed {path}"),
                        ));
                    }
                    // Served, this account just may not list this kind: a permission
                    // verdict, not an availability one.
                    Err(PrismError::Forbidden(_)) => {
                        return Ok(NamespaceStatus::served(
                            ns,
                            version,
                            format!("reachable ({path}: not permitted for this account)"),
                        ));
                    }
                    Err(PrismError::NotFound(_)) => continue,
                    // Any other 4xx (400 for a missing required parameter, 405, 412) proves
                    // the version is there.
                    Err(PrismError::Api {
                        status, message, ..
                    }) if status < 500 => {
                        return Ok(NamespaceStatus::served(
                            ns,
                            version,
                            format!("reachable ({path}: HTTP {status}: {message})"),
                        ));
                    }
                    Err(
                        e @ (PrismError::Auth
                        | PrismError::Connect { .. }
                        | PrismError::Tls(_)
                        | PrismError::Certificate { .. }
                        | PrismError::RateLimited { .. }
                        | PrismError::Transport(_)),
                    ) => return Err(e),
                    // Keep the first "down" reason: the shortest probe path is the most
                    // representative, and later candidates only repeat the outage. The path
                    // rides along, so the row says which endpoint was asked.
                    Err(e) => {
                        down.get_or_insert_with(|| down_detail(&path, &e));
                    }
                }
            }
            if let Some(detail) = down {
                return Ok(NamespaceStatus::down(ns, version, detail));
            }
        }
        let detail = if probed_any {
            format!("not served at {}", ns.versions.join(", "))
        } else {
            "no listable kind in catalog".to_string()
        };
        Ok(NamespaceStatus::not_served(ns, detail))
    }

    /// A poisoned routing table is taken as it stands, for the reason `buckets()` gives: the
    /// worst a panic under the guard can leave behind is one namespace's pin, and panicking a
    /// whole session's requests over it would be worse.
    pub(super) fn pins_guard(
        &self,
    ) -> std::sync::RwLockReadGuard<'_, HashMap<&'static str, &'static str>> {
        self.pins
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub(super) fn statuses_guard(&self) -> std::sync::RwLockReadGuard<'_, Vec<NamespaceStatus>> {
        self.statuses
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Whether this namespace's routing came from a restore rather than from a probe: the pins
    /// [`Client::adopt_pins`] resolved, and the namespaces that restore said nothing about.
    /// Those are the only ones the lazy repair may re-probe, and the only ones an absent
    /// status row reads as *unknown* for rather than as not served.
    pub(super) fn is_adopted(&self, namespace: &str) -> bool {
        self.adopted
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains(namespace)
    }
}
