use super::*;

impl StreamingClient {
    pub(super) async fn request_inner(
        &self,
        url: &str,
        user_agent: &str,
        cancel: &CancellationToken,
        mut output: Option<tokio::fs::File>,
        scoped: Option<(&RequestPolicy, Option<&RequestCredential>)>,
    ) -> Result<NetworkResponse, NetworkError> {
        let mut current = Url::parse(url)?;
        policy::reject_userinfo(&current)?;
        let public_policy = RequestPolicy::public(RequestPurpose::Package);
        let (request_policy, credential) = scoped.unwrap_or((&public_policy, None));
        let probe_origin = Origin::from_url(&current).ok();
        let mut credentials = policy::CredentialChain::new(request_policy, &current, credential);
        let original_https = current.scheme() == "https";
        let mut redirects = 0usize;
        loop {
            if cancel.is_cancelled() {
                return Err(NetworkError::Cancelled);
            }
            let resolved = tokio::select! {
                () = cancel.cancelled() => return Err(NetworkError::Cancelled),
                result = self.resolve_and_validate_policy(&current, Some(request_policy)) => result?,
            };
            let (host, port, addresses) = resolved;
            if original_https && current.scheme() != "https" {
                return Err(NetworkError::Downgrade);
            }
            let sockets: Vec<_> = addresses
                .iter()
                .map(|ip| SocketAddr::new(*ip, port))
                .collect();
            let client = tls::pinned_client(&host, &sockets, self.deadlines.connect)?;
            let mut request = client
                .get(current.clone())
                .header(reqwest::header::USER_AGENT, user_agent);
            if let Some(value) = credentials.header() {
                request = request.header(reqwest::header::AUTHORIZATION, value);
            }
            if request_policy.purpose == RequestPurpose::CatalogAsset {
                request = request.header(reqwest::header::ACCEPT, "application/octet-stream");
            }
            let response = tokio::select! {
                () = cancel.cancelled() => return Err(NetworkError::Cancelled),
                result = wait_for(
                    self.clock.as_ref(),
                    self.deadlines.idle,
                    request.send(),
                ) => {
                    match result? {
                        Ok(response) => response,
                        Err(error) if error.is_timeout() => return Err(NetworkError::Timeout),
                        Err(error) => return Err(error.into()),
                    }
                },
            };
            if response.status().is_redirection() {
                let next = redirect_target(
                    &current,
                    response.headers().get(reqwest::header::LOCATION),
                    redirects,
                    self.limits.max_redirects,
                )?;
                policy::reject_userinfo(&next)?;
                if request_policy.purpose == RequestPurpose::Probe
                    && Origin::from_url(&next).ok() != probe_origin
                {
                    return Err(NetworkError::InvalidPolicy);
                }
                credentials.redirect(&current, &next);
                current = next;
                redirects += 1;
                continue;
            }
            let status = response.status().as_u16();
            // Security and resource invariant: never poll a status-error body.
            if !(200..300).contains(&status) {
                return Ok(NetworkResponse {
                    status,
                    body: Bytes::new(),
                    final_url: current,
                });
            }
            if response
                .content_length()
                .is_some_and(|length| length > self.limits.max_bytes)
            {
                return Err(NetworkError::TooLarge(self.limits.max_bytes));
            }
            let mut stream = response.bytes_stream();
            let mut body = Vec::new();
            let mut count = 0u64;
            while let Some(chunk) = tokio::select! {
                () = cancel.cancelled() => return Err(NetworkError::Cancelled),
                result = wait_for(self.clock.as_ref(), self.deadlines.idle, stream.next()) => result?,
            } {
                let chunk = chunk?;
                count = count
                    .checked_add(chunk.len() as u64)
                    .ok_or(NetworkError::TooLarge(self.limits.max_bytes))?;
                if count > self.limits.max_bytes {
                    return Err(NetworkError::TooLarge(self.limits.max_bytes));
                }
                if let Some(file) = output.as_mut() {
                    tokio::select! {
                        () = cancel.cancelled() => return Err(NetworkError::Cancelled),
                        result = file.write_all(&chunk) => result?,
                    }
                } else {
                    body.extend_from_slice(&chunk);
                }
            }
            if let Some(file) = output.as_mut() {
                tokio::select! {
                    () = cancel.cancelled() => return Err(NetworkError::Cancelled),
                    result = file.flush() => result?,
                }
            }
            return Ok(NetworkResponse {
                status,
                body: Bytes::from(body),
                final_url: current,
            });
        }
    }

    #[cfg(test)]
    pub(super) async fn resolve_and_validate(
        &self,
        url: &Url,
    ) -> Result<(String, u16, Vec<IpAddr>), NetworkError> {
        self.resolve_and_validate_policy(url, None).await
    }

    pub(super) async fn resolve_and_validate_policy(
        &self,
        url: &Url,
        request_policy: Option<&RequestPolicy>,
    ) -> Result<(String, u16, Vec<IpAddr>), NetworkError> {
        policy::reject_userinfo(url)?;
        let host = url
            .host()
            .ok_or_else(|| NetworkError::Destination("URL has no host".into()))?;
        let host_text = match host {
            Host::Domain(value) => value.to_owned(),
            Host::Ipv4(value) => value.to_string(),
            Host::Ipv6(value) => value.to_string(),
        };
        let port = url
            .port_or_known_default()
            .ok_or_else(|| NetworkError::Destination("URL has no port".into()))?;
        let addresses = match host {
            Host::Ipv4(value) => vec![IpAddr::V4(value)],
            Host::Ipv6(value) => vec![normalize_ip(IpAddr::V6(value))],
            Host::Domain(_) => self
                .resolver
                .resolve(&host_text)
                .await
                .map_err(NetworkError::Resolve)?,
        };
        if addresses.is_empty() {
            return Err(NetworkError::Destination("empty DNS answer".into()));
        }
        let addresses: Vec<_> = addresses.into_iter().map(normalize_ip).collect();
        match &self.policy {
            DestinationPolicy::PublicOnly => {
                if url.scheme() != "https" {
                    return Err(NetworkError::Scheme);
                }
                if let Some(address) = addresses.iter().find(|address| {
                    !is_global_unicast(**address)
                        && !request_policy.is_some_and(|policy| policy.permits_private(url))
                }) {
                    return Err(NetworkError::Destination(address.to_string()));
                }
            }
            #[cfg(feature = "test-harness")]
            DestinationPolicy::LoopbackHarness(policy) => {
                if !policy
                    .reserved_hosts
                    .iter()
                    .any(|allowed| allowed.eq_ignore_ascii_case(&host_text))
                {
                    return Err(NetworkError::Destination(host_text));
                }
                if url.scheme() != "https" && url.scheme() != "http" {
                    return Err(NetworkError::Scheme);
                }
                if addresses
                    .iter()
                    .any(|ip| !policy.allowed_sockets.contains(&SocketAddr::new(*ip, port)))
                {
                    return Err(NetworkError::Destination(
                        "socket is outside harness allowlist".into(),
                    ));
                }
            }
        }
        Ok((host_text, port, addresses))
    }
}
