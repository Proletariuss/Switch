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
        let target = control::Target::request(request, "reader")?;
        let state = self.0.state.lock().map_err(|_| control::UNCONFIRMED)?;
        if target.daemon_epoch != self.0.sms_epoch {
            return Err(control::STALE);
        }
        if let Some(record) = state.record.as_ref().filter(|r| {
            r.control
                .target("reader", &self.0.sms_epoch, r.expected.reader_generation)
                == target
        }) {
            if request.method() == hyper::Method::POST {
                let claim = record
                    .claim
                    .try_is_current()
                    .map_err(|_| control::UNCONFIRMED)?;
                if !claim && !record.progress.borrow().cleanup_confirmed {
                    return Err(control::UNCONFIRMED);
                }
                let retained = record.clone();
                let completion = record.control.clone();
                record.control.begin(|| record.request_stop(), async move {
                    // The existing retained Handle mutex owns the one real
                    // worker join. Other observers cannot steal/drop its task.
                    let mut task = retained.task.lock().await;
                    match task.finish_cancel_safe().await {
                        Err(_) => completion.finish(
                            false,
                            false,
                            Some("原会话任务异常；清理未确认".into()),
                        ),
                        Ok(exit) if !exit.snapshot.cleanup_confirmed => completion.finish(
                            true,
                            false,
                            Some("原会话已join，但资源清理未确认".into()),
                        ),
                        Ok(_) => match retained.claim.release_confirmed() {
                            Ok(()) => completion.finish(true, true, None),
                            Err(_) => completion.finish(
                                true,
                                false,
                                Some("原会话已join，但claim释放未确认".into()),
                            ),
                        },
                    }
                })?;
            }
            return record.control.project(target);
        }
        if let Some((expected, owner)) = &state.last_stop
            && *expected == target
        {
            // Last-retired is read-only, including repeated POST after a lost ACK.
            return owner.project(target);
        }
        Err(control::STALE)
    }
}
