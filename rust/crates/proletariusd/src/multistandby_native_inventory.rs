//! Read-only discovery owned by Rust. The retained task holds the exact native
//! parent through actual card completion even if its HTTP waiter disappears.
use crate::{
    multistandby_card::{Failure, Service},
    reader_pcsc::Owner,
    reader_reservation::{Gate, NativeMultiBinding},
};
use bytes::Bytes;
use http_body_util::Full;
use hyper::{Request, Response, StatusCode, body::Incoming};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

pub(crate) const PATH: &str = "/api/internal/multistandby/inventory";
fn fail(code: &'static str, message: impl Into<String>) -> Failure {
    Failure {
        status: StatusCode::CONFLICT,
        code,
        message: message.into(),
    }
}
struct Cancel(Arc<AtomicBool>);
impl Drop for Cancel {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}
fn release(gate: &Gate, parent: &NativeMultiBinding) -> Result<(), Failure> {
    if gate.native_multi_current(parent) {
        gate.release_native_multi(parent)
            .map_err(|e| fail(e, "卡片清单资源释放未确认"))
    } else {
        match gate.release_native_multi_after_confirmed_loss(parent) {
            Ok(true) => Ok(()),
            Ok(false) => Err(fail(
                "physical_cleanup_unconfirmed",
                "读卡器已变化，物理资源释放尚未确认",
            )),
            Err(e) => Err(fail(e, "卡片清单资源释放未确认")),
        }
    }
}
// A transient gap between logical loss and its exact physical receipt must
// not orphan the parent. This retained worker is also the caller's cleanup join:
// cancellation and elapsed time never turn an unconfirmed release into success.
async fn release_confirmed(gate: &Gate, parent: &NativeMultiBinding) {
    loop {
        if release(gate, parent).is_ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
#[cfg(test)]
struct BeforeRelease {
    entered: tokio::sync::oneshot::Sender<()>,
    proceed: tokio::sync::oneshot::Receiver<()>,
}
pub(crate) async fn read(
    owner: Owner,
    gate: Arc<Gate>,
    service: Service,
    reader: String,
    generation: u64,
) -> Result<Value, Failure> {
    read_retained(
        owner,
        gate,
        service,
        reader,
        generation,
        #[cfg(test)]
        None,
    )
    .await
}
async fn read_retained(
    owner: Owner,
    gate: Arc<Gate>,
    service: Service,
    reader: String,
    generation: u64,
    #[cfg(test)] before_release: Option<BeforeRelease>,
) -> Result<Value, Failure> {
    let cancelled = Arc::new(AtomicBool::new(false));
    let _cancel = Cancel(cancelled.clone());
    tokio::spawn(async move {
        if cancelled.load(Ordering::Acquire){return Err(fail("cancelled","清单读取已取消"));}
        let parent=gate.acquire_native_multi(&reader,generation).map_err(|e|fail(e,"读卡器正在使用或选择已变化"))?;
        let operation=match gate.begin_native_multi_operation(owner.clone(),&parent) {
            Ok(operation)=>operation,
            Err(e)=>{release_confirmed(&gate,&parent).await;return Err(fail(e,"卡片清单事务未开始"));}
        };
        let work=async {
            gate.acquire_native_multi_pcsc(owner.clone(),&operation,cancelled.clone()).await.map_err(|e|fail(e.code,e.message))?;
            service.inventory_native(operation.transaction.clone(),operation.pcsc.clone(),cancelled.clone()).await
        };
        tokio::pin!(work);
        let result=loop {
            tokio::select! {
                result=&mut work=>break result,
                _=tokio::time::sleep(Duration::from_secs(20))=>{
                    if !cancelled.load(Ordering::Acquire) {
                        let valid=gate.renew_native_multi_operation(&operation).is_ok();
                        let physical=operation.pcsc.as_ref().is_none_or(|b|owner.renew(b,Duration::from_secs(120)).is_ok());
                        if !valid || !physical {cancelled.store(true,Ordering::Release);}
                    }
                }
            }
        };
        // The operation's own retained retirement keeps retrying real physical
        // cleanup. Keep its parent even if observing that receipt temporarily
        // fails; no error is proof that the card worker has ended.
        while gate.finish_native_multi_operation(&operation).await.is_err() {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        #[cfg(test)]
        if let Some(barrier)=before_release {
            let _=barrier.entered.send(());
            let _=barrier.proceed.await;
        }
        let current=gate.native_multi_current(&parent);
        release_confirmed(&gate,&parent).await;
        if !current || gate.selected_reader_generation()!=Some((reader.clone(),generation)) {return Err(fail("stale_reader_generation","读卡器选择已变化"));}
        if cancelled.load(Ordering::Acquire){return Err(fail("cancelled","清单读取已取消"));}
        let mut result=result?;
        result["reader"]=json!(reader);
        result["reader_generation"]=json!(generation);
        Ok(result)
    }).await.map_err(|_|fail("physical_cleanup_unconfirmed","卡片清单工作线程异常退出"))?
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Query {
    reader: String,
    reader_generation: u64,
}
pub(crate) async fn handle(
    request: Request<Incoming>,
    owner: Owner,
    gate: Arc<Gate>,
    service: Service,
) -> Response<Full<Bytes>> {
    if request.method() != hyper::Method::POST {
        return crate::json_response(
            StatusCode::METHOD_NOT_ALLOWED,
            &json!({"code":"method_not_allowed"}),
        );
    }
    let body = match crate::http_server::collect_body_with_timeout(
        request.into_body(),
        2048,
        Duration::from_secs(2),
    )
    .await
    {
        Ok(body) => body,
        _ => {
            return crate::json_response(
                StatusCode::BAD_REQUEST,
                &json!({"code":"invalid_request","error":"无效请求"}),
            );
        }
    };
    let query: Query = match serde_json::from_slice(&body) {
        Ok(query) => query,
        Err(_) => {
            return crate::json_response(
                StatusCode::BAD_REQUEST,
                &json!({"code":"invalid_request","error":"无效请求"}),
            );
        }
    };
    if query.reader.is_empty()
        || query.reader.len() > 512
        || query.reader.contains('\0')
        || query.reader_generation == 0
    {
        return crate::json_response(
            StatusCode::BAD_REQUEST,
            &json!({"code":"invalid_request","error":"无效请求"}),
        );
    }
    match read(owner, gate, service, query.reader, query.reader_generation).await {
        Ok(value) => crate::json_response(StatusCode::OK, &value),
        Err(error) => crate::json_response(
            error.status,
            &json!({"code":error.code,"error":error.message}),
        ),
    }
}
#[cfg(test)]
mod tests;
