use super::super::stop_control as control;
use super::*;

impl Service {
    pub(super) fn handle_stop_control<B: hyper::body::Body>(
        &self,
        request: &Request<B>,
    ) -> Response<Full<Bytes>> {
        control::response(self.stop_control(request))
    }
    fn stop_control<B: hyper::body::Body>(
        &self,
        request: &Request<B>,
    ) -> Result<control::Projection, &'static str> {
        let target = control::Target::request(request, "module")?;
        let state = self.0.state.lock().map_err(|_| control::UNCONFIRMED)?;
        if target.daemon_epoch != self.0.epoch {
            return Err(control::STALE);
        }
        if let Some(record) = state
            .record
            .as_ref()
            .filter(|r| r.control.target("module", &self.0.epoch, r.id) == target)
        {
            if request.method() == hyper::Method::POST {
                let claim = record
                    .claim
                    .try_is_current()
                    .map_err(|_| control::UNCONFIRMED)?;
                let status = record.state.lock().map_err(|_| control::UNCONFIRMED)?;
                if !claim && !status.retired {
                    return Err(control::UNCONFIRMED);
                }
                let retained = record.clone();
                let resources = self.clone();
                let completion = record.control.clone();
                drop(status);
                record.control.begin(
                    || {
                        // Reservation succeeded before any business mutation.
                        // This short guard ends before the join task is spawned.
                        let mut status =
                            record.state.lock().expect("module stop state unavailable");
                        status.stopping = true;
                        record.stop.send_replace(true);
                    },
                    async move {
                        if resources.release_provisional_if_any(&retained).is_err() {
                            completion.finish(false, false, Some("原模块claim释放未确认".into()));
                            return;
                        }
                        match retained.join.wait().await {
                            Ok(joined) => {
                                let cleanup = retained
                                    .state
                                    .lock()
                                    .map(|s| s.retired && !s.provisional)
                                    .unwrap_or(false)
                                    && matches!(retained.claim.try_is_current(), Ok(false));
                                completion.finish(
                                    joined,
                                    cleanup,
                                    (!(joined && cleanup))
                                        .then(|| "原模块owner join/cleanup尚未全部确认".into()),
                                );
                            }
                            Err(_) => completion.finish(
                                false,
                                false,
                                Some("原模块缺少权威join结果；资源保持隔离".into()),
                            ),
                        }
                    },
                )?;
            }
            return record.control.project(target);
        }
        if let Some((expected, owner)) = &state.last_stop
            && *expected == target
        {
            return owner.project(target);
        }
        Err(control::STALE)
    }
}
