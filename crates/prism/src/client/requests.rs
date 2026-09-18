//! The requests: lists, gets, actions, and the status-to-error mapping.

use super::*;

/// Ceiling for a total-less walk: a server that never returns a short page must not
/// spin this loop forever.
pub(super) const MAX_PAGES: u32 = 10_000;

/// The rate-budget key every [`Client::get_path`] shares. Its path is a runtime string, so it
/// has no catalog template to key a bucket by, and one key for the lot is the honest name for
/// what it is: a single caller, the Disaster Recovery sampler, which paces itself.
pub(super) const GET_PATH_TEMPLATE: &str = "GET (path)";

/// The widest this client ever runs, whatever a Prism Central advertises. The narrower bound is
/// the host tier itself: see [`Client::fan_out`].
pub(super) const MAX_FAN_OUT: usize = 4;

/// Top-level keys a v4 PUT must not carry back. Nested `$objectType` keys stay: they carry the
/// polymorphic discriminator a v4 body needs. `ownerUuid` is absent, because the category PUT
/// requires it.
pub(super) const READ_ONLY_KEYS: &[&str] = &[
    "$objectType",
    "$reserved",
    "$metadata",
    "extId",
    "links",
    "tenantId",
    "createdBy",
    "creationTime",
    "lastModifiedTime",
];

/// The one error both ETag paths raise: no validator, so no conditional request to build.
pub(super) fn missing_etag(kind: &Kind, ext_id: &str) -> PrismError {
    PrismError::Decode(format!("{} {ext_id} returned no ETag header", kind.id))
}

/// `data` read as the task reference an accepted mutation returns. A v4 answer that names itself
/// settles it, because every *entity* carries `extId` too: an action whose 200 hands back the
/// updated entity must not mint a task the watcher would then poll for a task that never existed.
/// An answer with no `$objectType` falls back to the bare `extId` rule.
pub(super) fn task_ref(data: &Value) -> Option<TaskRef> {
    let names_a_task = match data.get("$objectType").and_then(Value::as_str) {
        Some(ty) => ty.ends_with("TaskReference"),
        None => true,
    };
    if !names_a_task {
        return None;
    }
    data.get("extId")
        .and_then(Value::as_str)
        .map(|id| TaskRef { ext_id: id.into() })
}

/// The catalog's guess, degraded by what actually arrived. 202 is still not required: the
/// client trusts the envelope, not the status.
pub(super) fn result_of(
    returns: ActionReturn,
    data: Value,
    kind: &str,
    action: &str,
) -> ActionResult {
    let as_task = task_ref(&data);
    match (returns, as_task) {
        (ActionReturn::Task, Some(t)) => ActionResult::Task(t),
        (ActionReturn::Task, None) if data.is_null() => ActionResult::Empty,
        (ActionReturn::Task, None) => {
            tracing::debug!(kind, action, "catalog said Task, answer carried a document");
            ActionResult::Payload(data)
        }
        (ActionReturn::Payload | ActionReturn::None, _) if data.is_null() => ActionResult::Empty,
        (ActionReturn::None, _) => {
            tracing::debug!(kind, action, "catalog said None, answer carried a document");
            ActionResult::Payload(data)
        }
        (ActionReturn::Payload, _) => ActionResult::Payload(data),
    }
}

/// `kind`'s path with `parents` (and `tail`, the entity for a get path) filled in. A count
/// mismatch is a programming error, reported as `Catalog` naming the kind, the template, and
/// both counts.
pub(super) fn scoped_path(
    kind: &Kind,
    template: &str,
    parents: &[&str],
    tail: Option<&str>,
) -> Result<String, PrismError> {
    let mut values: Vec<&str> = parents.to_vec();
    values.extend(tail);
    fill_placeholders(template, &values).ok_or_else(|| {
        PrismError::Catalog(format!(
            "{}: {template} needs {} path value(s), got {}",
            kind.id,
            placeholder_count(template),
            values.len()
        ))
    })
}

/// Candidates as `name (extId)`, for an error that has to tell the user what to pick from.
/// An empty list reads `(none)` rather than trailing off after the colon.
/// Not named `describe`: that is the error-chain helper this module already imports.
pub(super) fn name_and_id<'a>(cs: impl IntoIterator<Item = &'a Entity>) -> String {
    let listed = cs
        .into_iter()
        .map(|c| format!("{} ({})", c.name, c.ext_id))
        .collect::<Vec<_>>()
        .join(", ");
    if listed.is_empty() {
        "(none)".to_string()
    } else {
        listed
    }
}

/// Pages of `limit` items needed to cover `total`, so page indices run `0..page_count`.
/// Saturating, not truncating. The `as u32` this replaces wrapped a total too large to walk
/// down to a small page count, which is exactly the number that would slip past `list_all`'s
/// ceiling: 2^63-1 rows at a hundred a page came out as 2,061,584,303 pages, and a total one
/// page wider would have come out as a handful.
pub(super) fn page_count(total: u64, limit: u64) -> u32 {
    u32::try_from(total.div_ceil(limit.max(1))).unwrap_or(u32::MAX)
}

impl Client {
    pub async fn list_page(
        &self,
        kind: &'static Kind,
        page: u32,
        opts: &ListOptions,
    ) -> Result<Page, PrismError> {
        self.list_page_at(kind, page, opts, None).await
    }

    /// `list_page` at an explicit API version, bypassing the pins: how negotiation probes a
    /// version. `None` means the catalog path, rewritten through the pins as usual.
    pub(super) async fn list_page_at(
        &self,
        kind: &'static Kind,
        page: u32,
        opts: &ListOptions,
        version: Option<&str>,
    ) -> Result<Page, PrismError> {
        let limit = opts.limit.clamp(1, PAGE_LIMIT_MAX);
        let mut query: Vec<(&str, String)> = Vec::new();
        if kind.list_params.page {
            query.push(("$page", page.to_string()));
        }
        if kind.list_params.limit {
            query.push(("$limit", limit.to_string()));
        }
        if let (true, Some(f)) = (kind.list_params.filter, &opts.filter) {
            query.push(("$filter", f.clone()));
        }
        if let (true, Some(o)) = (kind.list_params.orderby, &opts.orderby) {
            query.push(("$orderby", o.clone()));
        }
        let parents: Vec<&str> = opts.parents.iter().map(String::as_str).collect();
        let scoped = scoped_path(kind, kind.list_path, &parents, None)?;
        let path = match version {
            Some(v) => with_version(&scoped, v),
            None => self.pinned_path_for(kind, &scoped),
        };
        // The narrowing the caller asked for, if this endpoint declares one and this Prism
        // Central has not already refused it. Never `kind.select`: what is narrowed is the
        // scheduler's decision, and a `$select` applied to every list from down here would
        // reach `can_i` and the version probes, which come through with default options.
        let mut select = match (kind.list_params.select, &opts.select) {
            (true, Some(s)) if !self.select_refused(kind) => Some(s.clone()),
            _ => None,
        };
        loop {
            let mut query = query.clone();
            if let Some(s) = &select {
                query.push(("$select", s.clone()));
            }
            let req = self.http.get(self.raw_url(&path)).query(&query);
            // After `scoped_path`: a caller error must not cost a token, still less a whole
            // window asleep before it is reported. `send` is what spends it.
            let (status, headers, body) = self
                .send(&HttpMethod::GET, kind.list_path, kind.rate, req)
                .await?;
            // The extraction is inside the same arm: a body that parses as JSON and carries no
            // usable `data` is a `Decode`, and it is raised here rather than by `ok` so that
            // the caller sees one error for the request rather than a success it cannot read.
            let e = match Self::ok(status, &headers, &body, &path).and_then(envelope::list_items) {
                Ok((items, total)) => {
                    return Ok(Page {
                        entities: items
                            .into_iter()
                            .map(|v| Entity::new(kind, v, None))
                            .collect(),
                        total,
                    });
                }
                Err(e) => e,
            };
            // A 400 on a list carrying `$select` is this Prism Central saying the narrowing is
            // wrong for this endpoint, whatever the spec declared. The table is worth more than
            // the bytes: ask for the whole document instead, once, and remember for the rest of
            // the session so the next cycle does not spend a request rediscovering it.
            if select.is_some() && matches!(e, PrismError::Api { status: 400, .. }) {
                self.refuse_select(kind);
                select = None;
                continue;
            }
            // `version.is_none()` excludes the probes themselves: a 404 during negotiation is
            // the answer negotiation is asking for, not a pin to repair.
            if version.is_none() {
                self.repair_after(kind.namespace, &e).await;
            }
            return Err(e);
        }
    }

    /// Page 0 first, then the remaining pages with bounded concurrency, in order.
    pub async fn list_all(
        &self,
        kind: &'static Kind,
        opts: &ListOptions,
    ) -> Result<Vec<Entity>, PrismError> {
        let first = self.list_page(kind, 0, opts).await?;
        let limit = u64::from(opts.limit.clamp(1, PAGE_LIMIT_MAX));
        let mut all = first.entities;
        if !kind.list_params.page {
            return Ok(all);
        }
        let limit_usize = limit as usize;
        match first.total {
            // A total we can trust: fan the remaining pages out, then keep them in page order.
            Some(total) if total > limit => {
                let pages = page_count(total, limit);
                // The same ceiling the total-less walk below has, on the branch that lacked
                // it. A Prism Central that miscounts its own list is the one input this arm
                // cannot survive: it fanned out over every page the total named, which for a
                // `totalAvailableResults` of 2^63-1 is an endless crawl at the pacing's three
                // requests a second, with nothing on the screen to say so.
                if pages > MAX_PAGES {
                    return Err(PrismError::Decode(format!(
                        "{} reported {total} rows, which is more than {MAX_PAGES} pages",
                        kind.id
                    )));
                }
                let rest: Vec<Page> = stream::iter(1..pages)
                    .map(|p| self.list_page(kind, p, opts))
                    .buffered(self.fan_out())
                    .try_collect()
                    .await?;
                for page in rest {
                    all.extend(page.entities);
                }
            }
            Some(_) => {}
            // No total: walk one page at a time until a short or empty page ends the list.
            None => {
                let mut page = 1;
                while all.len() % limit_usize == 0 && !all.is_empty() {
                    if page >= MAX_PAGES {
                        return Err(PrismError::Decode(format!(
                            "{} returned more than {MAX_PAGES} pages without a total",
                            kind.id
                        )));
                    }
                    let next = self.list_page(kind, page, opts).await?;
                    if next.entities.is_empty() {
                        break;
                    }
                    all.extend(next.entities);
                    page += 1;
                }
            }
        }
        Ok(all)
    }

    /// Match a cluster by name (exact, then case-insensitive). Zero or several matches are errors
    /// that list the candidates.
    pub async fn resolve_cluster(&self, name: &str) -> Result<ClusterRef, PrismError> {
        let kind = nutsh_catalog::kind("clustermgmt.config.Cluster")
            .ok_or_else(|| PrismError::Catalog("catalog has no cluster kind".into()))?;
        let clusters = self.list_all(kind, &ListOptions::default()).await?;
        let exact: Vec<&Entity> = clusters.iter().filter(|c| c.name == name).collect();
        let matches = if exact.is_empty() {
            clusters
                .iter()
                .filter(|c| c.name.eq_ignore_ascii_case(name))
                .collect::<Vec<_>>()
        } else {
            exact
        };
        match matches.as_slice() {
            [one] => Ok(ClusterRef {
                ext_id: one.ext_id.clone(),
                name: one.name.clone(),
            }),
            // Not `NotFound`: nothing 404'd, the list came back fine and held no such name.
            [] => Err(PrismError::Unresolved(format!(
                "cluster {name:?} not found; known: {}",
                name_and_id(&clusters)
            ))),
            many => Err(PrismError::Ambiguous(format!(
                "cluster {name:?} is ambiguous: {}",
                name_and_id(many.iter().copied())
            ))),
        }
    }

    pub async fn get(&self, kind: &'static Kind, ext_id: &str) -> Result<Entity, PrismError> {
        self.get_in(kind, &[], ext_id).await
    }

    /// `get` for a nested kind: `parents` fill the path's leading placeholders, `ext_id` the last.
    pub async fn get_in(
        &self,
        kind: &'static Kind,
        parents: &[String],
        ext_id: &str,
    ) -> Result<Entity, PrismError> {
        let template = kind
            .get_path
            .ok_or_else(|| PrismError::Catalog(format!("{} has no get endpoint", kind.id)))?;
        let parents: Vec<&str> = parents.iter().map(String::as_str).collect();
        let path = scoped_path(kind, template, &parents, Some(ext_id))?;
        let path = self.pinned_path_for(kind, &path);
        // The kind's tier is the list GET's; the catalog declares no separate one for the
        // read, and it is the closest budget there is.
        let (status, headers, body) = self
            .send(
                &HttpMethod::GET,
                template,
                kind.rate,
                self.http.get(self.raw_url(&path)),
            )
            .await?;
        let env = Self::ok(status, &headers, &body, &path)?;
        // `If-Match` uses strong comparison, so a weak validator is stored without its `W/`
        // prefix: sending `W/"x"` back would never match.
        let etag = headers
            .get(header::ETAG)
            .and_then(|v| v.to_str().ok())
            .map(|v| v.strip_prefix("W/").unwrap_or(v).to_string());
        Ok(Entity::new(kind, envelope::one(env)?, etag))
    }

    /// A GET at an API path the catalog does not name, returning the envelope's `data`. For the
    /// DR sampler: `protected-resources/{extId}` is get-by-id only, so the kind is not in the
    /// catalog. Version-pinned like any other path.
    pub async fn get_path(&self, path: &str) -> Result<Value, PrismError> {
        let path = self.pinned_path(path);
        // Every request passes the host ceiling first, whatever its own tier says, and the
        // operation's own budget has to exist for a 429 to drain it. The tier is the catalog's
        // default because there is no kind here to read one from - the sampler's one request
        // every 200 ms never reaches it either way.
        let (status, headers, body) = self
            .send(
                &HttpMethod::GET,
                GET_PATH_TEMPLATE,
                RateLimit::DEFAULT,
                self.http.get(self.raw_url(&path)),
            )
            .await?;
        let env = Self::ok(status, &headers, &body, &path)?;
        envelope::one(env)
    }

    /// Run a catalog action. `parents` fill the path's leading placeholders and `ext_id` the
    /// entity's; a count mismatch is a programming error, reported as `Catalog`. Fetches the
    /// entity first when the action needs an ETag.
    pub async fn act(
        &self,
        kind: &'static Kind,
        parents: &[String],
        ext_id: &str,
        action: &str,
        body: Option<Value>,
    ) -> Result<ActionResult, PrismError> {
        self.act_with_etag(kind, parents, ext_id, action, body, None)
            .await
    }

    /// `act` with the validator already in hand. A caller that read the entity itself - `update`
    /// builds its body from one - passes the ETag of *that* read: letting this method fetch a
    /// second, newer one would send the older body under a fresh `If-Match`, so a write that
    /// landed in between would be overwritten rather than refused with a 412. `None` means
    /// "fetch it here", which is what `act` asks for.
    pub(super) async fn act_with_etag(
        &self,
        kind: &'static Kind,
        parents: &[String],
        ext_id: &str,
        action: &str,
        body: Option<Value>,
        etag: Option<String>,
    ) -> Result<ActionResult, PrismError> {
        let a = kind
            .action(action)
            .ok_or_else(|| PrismError::Catalog(format!("{} has no action {action}", kind.id)))?;
        if a.needs_body && body.is_none() {
            return Err(PrismError::Catalog(format!(
                "{action} on {} requires a request body",
                kind.id
            )));
        }
        if !a.takes_body && body.is_some() {
            return Err(PrismError::Catalog(format!(
                "{action} on {} takes no request body",
                kind.id
            )));
        }
        // Not `scope`: `Action::scope` is the list of placeholder *names*, this is the ids.
        let parent_ids: Vec<&str> = parents.iter().map(String::as_str).collect();
        // `scoped_path` already names the kind, the template and both counts; only the action
        // is missing, and a caller that passed the wrong number of ids needs to know which.
        let filled = scoped_path(
            kind,
            a.path,
            &parent_ids,
            a.names_entity().then_some(ext_id),
        )
        .map_err(|e| match e {
            PrismError::Catalog(m) => PrismError::Catalog(format!("{action} on {m}")),
            other => other,
        })?;
        let path = self.pinned_path_for(kind, &filled);
        let method = match a.method {
            Method::Get => HttpMethod::GET,
            Method::Post => HttpMethod::POST,
            Method::Put => HttpMethod::PUT,
            Method::Patch => HttpMethod::PATCH,
            Method::Delete => HttpMethod::DELETE,
        };
        let mut req = self
            .http
            .request(method.clone(), self.raw_url(&path))
            .header("NTNX-Request-Id", uuid::Uuid::new_v4().to_string());
        if a.needs_etag {
            let etag = match etag {
                Some(tag) => tag,
                None => {
                    // Say which fetch failed: a 404 here is the entity, not the action endpoint.
                    let current =
                        self.get_in(kind, parents, ext_id)
                            .await
                            .map_err(|e| match e {
                                PrismError::NotFound(m) => PrismError::NotFound(format!(
                                    "{} {ext_id} (before {action}): {m}",
                                    kind.id
                                )),
                                other => other,
                            })?;
                    current.etag.ok_or_else(|| missing_etag(kind, ext_id))?
                }
            };
            req = req.header(header::IF_MATCH, etag);
        }
        if let Some(b) = &body {
            req = req.json(b);
        }
        // After the refusals and after the ETag read, so a token is spent only once this
        // request is certain to go out: a 404 on that read must not cost the action one.
        let (status, headers, resp) = self.send(&method, a.path, a.rate, req).await?;
        // Only `act` knows what was being changed, so it builds the 412 error itself.
        if status == StatusCode::PRECONDITION_FAILED {
            return Err(PrismError::Conflict {
                kind: kind.id,
                ext_id: ext_id.to_string(),
                action: action.to_string(),
            });
        }
        let env = Self::ok(status, &headers, &resp, &path)?;
        Ok(result_of(a.returns, env.data, kind.id, action))
    }

    /// Fetch, merge `changes` over the top-level keys (a nested object is replaced, not merged),
    /// strip the read-only keys, and PUT the whole object: v4 PUT is full-replace.
    pub async fn update(
        &self,
        kind: &'static Kind,
        parents: &[String],
        ext_id: &str,
        changes: Value,
    ) -> Result<ActionResult, PrismError> {
        // Refused before the GET: anything but an object would merge nothing and PUT the entity
        // back unchanged, which burns a task and reads as success.
        let Value::Object(changes) = changes else {
            return Err(PrismError::Catalog(format!(
                "{} {ext_id}: update changes must be a JSON object",
                kind.id
            )));
        };
        let current = self.get_in(kind, parents, ext_id).await?;
        // One read serves both halves, the body and the `If-Match`; every `update` action in the
        // catalog is a PUT that needs one, so a missing ETag is an error here, not a second GET.
        let etag = Some(current.etag.ok_or_else(|| missing_etag(kind, ext_id))?);
        let mut body = current.raw;
        let Some(object) = body.as_object_mut() else {
            return Err(PrismError::Decode(format!(
                "{} {ext_id} is not an object",
                kind.id
            )));
        };
        object.extend(changes);
        // After the merge, so `changes` cannot smuggle `extId` or `$objectType` back into the body.
        for key in READ_ONLY_KEYS {
            object.remove(*key);
        }
        self.act_with_etag(kind, parents, ext_id, "update", Some(body), etag)
            .await
    }

    /// `delete` is modelled as an action, so callers need not know that DELETE is one.
    pub async fn delete(
        &self,
        kind: &'static Kind,
        parents: &[String],
        ext_id: &str,
    ) -> Result<ActionResult, PrismError> {
        self.act(kind, parents, ext_id, "delete", None).await
    }

    /// 412 stays an `Api` error here: only the caller knows which entity and action it meant,
    /// so `act` intercepts that status before this runs. `path` stands in for the message when
    /// the body carries none: a bare "HTTP 404" would only repeat what the variant prints.
    pub(super) fn ok(
        status: StatusCode,
        headers: &HeaderMap,
        body: &[u8],
        path: &str,
    ) -> Result<envelope::Envelope, PrismError> {
        let message = || envelope::error_message(body).unwrap_or_else(|| path.to_string());
        match status.as_u16() {
            200..=299 => envelope::parse(body),
            401 => Err(PrismError::Auth),
            403 => Err(PrismError::Forbidden(message())),
            404 => Err(PrismError::NotFound(message())),
            429 => Err(PrismError::RateLimited {
                retry_after: retry_after(headers),
            }),
            // A 5xx is "not now", so the header that says how long is kept with it. `ok` is
            // the only place both the status and the headers are in hand.
            s => Err(PrismError::Api {
                status: s,
                message: message(),
                retry_after: headers
                    .contains_key(header::RETRY_AFTER)
                    .then(|| retry_after(headers)),
            }),
        }
    }

    /// Keep the whole source chain: reqwest's Display hides "invalid peer certificate: UnknownIssuer".
    pub(super) fn map_transport(&self, e: reqwest::Error) -> PrismError {
        if let Some(reason) = certificate_error(&e) {
            return PrismError::Certificate {
                host: self.host.clone(),
                reason,
            };
        }
        if e.is_connect() || e.is_timeout() {
            PrismError::Connect {
                host: self.host.clone(),
                message: describe(&e),
            }
        } else {
            PrismError::Transport(describe(&e))
        }
    }
}
