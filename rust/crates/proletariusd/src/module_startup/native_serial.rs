//! One retained serial lifecycle, independent of the compatibility child.
//! Selection is stable even when opening/initializing the chosen port fails.
use super::*;
use crate::modem_actor::NativeSerialSession;

const BAUD: u32 = 115_200;
const METADATA_INTERVAL: Duration = Duration::from_secs(15);

pub(super) async fn run(
    service: &Service,
    modem: ModemActor,
    radio: module_radio::Service,
    inbox: InboxState,
    producer: String,
    port: String,
    archive: ArchiveSource,
    mut stop: watch::Receiver<bool>,
    mut finalize: watch::Receiver<bool>,
) -> Result<(), StartupError> {
    // Claim and durable receive ownership precede both the first open and the
    // selection receipt that permits main to launch a managed Go child.
    loop {
        if *stop.borrow() {
            return finalize_serial(&modem, &mut finalize).await;
        }
        let prepared = prepare(&modem, &inbox, &producer, &archive).await;
        match prepared {
            Ok(()) => break,
            Err(code) => {
                service.error(Some(code));
                service.0.selection.send_replace(Selection::Failed { code });
                retry(&mut stop).await;
            }
        }
    }
    service.error(None);
    service
        .0
        .selection
        .send_replace(Selection::Serial { port: port.clone() });

    let mut bootstrapped = None;
    let mut next_metadata = tokio::time::Instant::now();
    loop {
        if *stop.borrow() {
            break;
        }
        let observed = modem.native_serial_observation();
        // Call-mode can intentionally replace the physical session. Adopt the
        // actor's exact new handle only after the radio's retained work settles;
        // never reopen a port while that operation owns the shared channel.
        if radio.sim_channel_busy() {
            retry(&mut stop).await;
            continue;
        }
        let session = match modem.native_serial_session() {
            Some(session) => session,
            None => match modem.open_native_serial(port.clone(), BAUD).await {
                Ok(session) => session,
                Err(error) => {
                    service.error(Some(error.code()));
                    retry(&mut stop).await;
                    continue;
                }
            },
        };
        if *stop.borrow() {
            break;
        }
        if bootstrapped != Some(session.serial_session) {
            match bootstrap(&modem, &radio, &session, &stop).await {
                Ok(true) => {
                    bootstrapped = Some(session.serial_session);
                    next_metadata = tokio::time::Instant::now() + METADATA_INTERVAL;
                    service.error(None);
                }
                Ok(false) => break,
                Err(error) => {
                    service.error(Some(error.code));
                    if error.retire && !*stop.borrow() {
                        // An initialization failure is cached for its session.
                        // Retry it only after that exact port has truly closed.
                        if let Err(close) = modem.close_native_serial(&session).await {
                            service.error(Some(close.code()));
                        }
                    }
                    retry(&mut stop).await;
                    continue;
                }
            }
        } else if observed.refresh_needed || tokio::time::Instant::now() >= next_metadata {
            match modem.refresh_native_serial_metadata(&session).await {
                Ok(_) => {
                    if *stop.borrow() {
                        break;
                    }
                    // A SIM probe revokes receive admission. Restore it only
                    // after this same-session metadata result has settled;
                    // strict SMS readiness still requires a confirmed SIM.
                    if let Err(error) = modem.commit_native_serial_ready(&session).await {
                        service.error(Some(error.code()));
                        retry(&mut stop).await;
                        continue;
                    }
                    service.error(None);
                    next_metadata = tokio::time::Instant::now() + METADATA_INTERVAL;
                }
                Err(error) => {
                    // Busy keeps the previous generation-safe cache. There is
                    // one refresh owner, bounded retries, and no raw AT fallback.
                    service.error(Some(error.code()));
                    retry(&mut stop).await;
                    continue;
                }
            }
        }
        if *stop.borrow() {
            break;
        }
        let latest = modem.native_serial_observation();
        if latest.refresh_needed || latest.serial_session != bootstrapped {
            continue;
        }
        let revision = latest.revision;
        tokio::select! {
            _ = stop.changed() => {},
            _ = modem.wait_native_serial_change(revision) => {},
            _ = tokio::time::sleep_until(next_metadata) => {},
        }
    }
    // begin_shutdown freezes this loop only. Main drains business and radio
    // owners before shutdown sends finalize; no early physical close occurs.
    finalize_serial(&modem, &mut finalize).await
}

async fn prepare(
    modem: &ModemActor,
    inbox: &InboxState,
    producer: &str,
    archive: &ArchiveSource,
) -> Result<(), &'static str> {
    modem
        .claim_native_serial_management()
        .map_err(|error| error.code())?;
    inbox
        .bind_native_module_producer(producer.to_owned())
        .map_err(|_| "module_startup_inbox_unavailable")?;
    let store = archive.store()?;
    let checked = store.clone();
    tokio::task::spawn_blocking(move || checked.load())
        .await
        .map_err(|_| "module_startup_archive_unavailable")?
        .map_err(|_| "module_startup_archive_unavailable")?;
    modem.ensure_sms_receive_worker(store);
    Ok(())
}

struct StageError {
    code: &'static str,
    retire: bool,
}
async fn bootstrap(
    modem: &ModemActor,
    radio: &module_radio::Service,
    session: &NativeSerialSession,
    stop: &watch::Receiver<bool>,
) -> Result<bool, StageError> {
    if !session.observation().initialized {
        let initialized = modem
            .initialize_native_serial(session)
            .await
            .map_err(|error| StageError {
                code: error.code(),
                retire: !matches!(
                    error.code(),
                    "serial_command_busy"
                        | "serial_actor_busy"
                        | "serial_initialization_busy"
                        | "serial_card_busy"
                        | "serial_card_binding_changed"
                ),
            })?;
        if !initialized.completed || !initialized.probe_succeeded {
            return Err(StageError {
                code: "native_serial_initialization_incomplete",
                retire: true,
            });
        }
    }
    if *stop.borrow() {
        return Ok(false);
    }
    radio
        .configure_native_serial(session.serial_session)
        .await
        .map_err(|error| StageError {
            code: error.public_code(),
            retire: false,
        })?;
    if *stop.borrow() {
        return Ok(false);
    }
    radio
        .bootstrap_native_serial(session.serial_session)
        .await
        .map_err(|error| StageError {
            code: error.public_code(),
            retire: false,
        })?;
    if *stop.borrow() {
        return Ok(false);
    }
    modem
        .refresh_native_serial_metadata(session)
        .await
        .map_err(|error| StageError {
            code: error.code(),
            retire: false,
        })?;
    if *stop.borrow() {
        return Ok(false);
    }
    modem
        .commit_native_serial_ready(session)
        .await
        .map_err(|error| StageError {
            code: error.code(),
            retire: false,
        })?;
    Ok(true)
}

async fn finalize_serial(
    modem: &ModemActor,
    finalize: &mut watch::Receiver<bool>,
) -> Result<(), StartupError> {
    while !*finalize.borrow() {
        if finalize.changed().await.is_err() {
            return Err(StartupError {
                code: "native_serial_finalize_unconfirmed",
            });
        }
    }
    if let Some(session) = modem.native_serial_session() {
        modem
            .close_native_serial(&session)
            .await
            .map_err(|error| StartupError { code: error.code() })?;
        let terminal = session.observation().terminal;
        if !terminal.is_some_and(|terminal| {
            terminal.physical_closed
                && terminal.serial_session == session.serial_session
                && terminal.owner_epoch == session.owner_epoch
        }) {
            return Err(StartupError {
                code: "native_serial_close_unconfirmed",
            });
        }
    }
    modem
        .finish_sms_receiver()
        .await
        .map_err(|error| StartupError { code: error.code() })?;
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests;
