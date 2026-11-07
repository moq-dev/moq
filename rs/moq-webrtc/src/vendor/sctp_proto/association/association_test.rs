use crate::chunk::chunk_i_forward_tsn::ChunkIForwardTsnStream;
use crate::chunk::{Chunk, chunk_init::ChunkInit};
use crate::config::DEFAULT_SCTP_PORT;
use crate::config::generate_snap_token;

use super::*;

const ACCEPT_CH_SIZE: usize = 16;

fn create_association(config: TransportConfig) -> Association {
    Association::new(
        None,
        Arc::new(config),
        1400,
        0,
        SocketAddr::from_str("0.0.0.0:0").unwrap(),
        None,
        Instant::now(),
    )
}

#[test]
fn test_assoc_is_closing() {
    let closing_states = [
        AssociationState::ShutdownSent,
        AssociationState::ShutdownAckSent,
        AssociationState::ShutdownPending,
        AssociationState::ShutdownReceived,
    ];

    for state in [
        AssociationState::Closed,
        AssociationState::CookieWait,
        AssociationState::CookieEchoed,
        AssociationState::Established,
    ] {
        let a = Association {
            state,
            ..Default::default()
        };

        assert!(!a.is_closing(), "{state} should not be closing");
    }

    for state in closing_states {
        let a = Association {
            state,
            ..Default::default()
        };

        assert!(a.is_closing(), "{state} should be closing");
        assert!(!a.is_closed(), "{state} should not be closed");
    }
}

fn outgoing_reset(rsn: u32, stream_id: StreamId) -> ChunkReconfig {
    ChunkReconfig {
        param_a: Some(Box::new(ParamOutgoingResetRequest {
            reconfig_request_sequence_number: rsn,
            stream_identifiers: vec![stream_id],
            ..Default::default()
        })),
        ..Default::default()
    }
}

fn insert_active_reset(a: &mut Association, rsn: u32, stream_id: StreamId) {
    a.reconfigs.insert(rsn, outgoing_reset(rsn, stream_id));
    a.active_reconfig = Some(rsn);
}

fn insert_queued_reset(a: &mut Association, rsn: u32, stream_id: StreamId) {
    let reset = outgoing_reset(rsn, stream_id);
    a.reconfigs.insert(rsn, reset.clone());
    let packet = a.create_packet(vec![Box::new(reset)]);
    a.control_queue.push_back(packet);
}

fn reconfig_response_result(packets: &[Packet], rsn: u32) -> Option<ReconfigResult> {
    packets.iter().find_map(|packet| {
        packet.chunks.iter().find_map(|chunk| {
            let reconfig = chunk.as_any().downcast_ref::<ChunkReconfig>()?;
            reconfig
                .param_a
                .iter()
                .chain(reconfig.param_b.iter())
                .find_map(|param| {
                    let response = param.as_any().downcast_ref::<ParamReconfigResponse>()?;
                    (response.reconfig_response_sequence_number == rsn).then_some(response.result)
                })
        })
    })
}

#[test]
fn test_reconfig_in_progress_timeout_does_not_consume_retry_budget() -> Result<()> {
    let now = Instant::now();
    let rsn = 7;
    let mut a = create_association(
        TransportConfig::default()
            .with_max_init_retransmits(Some(0))
            .with_rto_initial_ms(1),
    );

    insert_active_reset(&mut a, rsn, 1);
    a.timers.start(Timer::Reconfig, now, a.rto_mgr.get_rto());

    let response: Box<dyn Param + Send + Sync> = Box::new(ParamReconfigResponse {
        reconfig_response_sequence_number: rsn,
        result: ReconfigResult::InProgress,
    });
    a.handle_reconfig_param(&response, &mut vec![])?;

    // The outbound path restarts the timer after processing InProgress. RFC
    // 6525 section 5.2.7 H2 requires the next expiry to retransmit without
    // incrementing the error counter.
    a.timers
        .restart_if_stale(Timer::Reconfig, now, a.rto_mgr.get_rto());
    let deadline = a.timers.get(Timer::Reconfig).unwrap();
    a.handle_timeout(deadline);

    assert!(a.reconfigs.contains_key(&rsn));
    assert!(a.will_retransmit_reconfig);
    Ok(())
}

#[test]
fn test_reset_complete_only_for_successful_reconfig_response() -> Result<()> {
    let rsn = 7;
    let stream_id = 1;

    for result in [
        ReconfigResult::SuccessNop,
        ReconfigResult::SuccessPerformed,
        ReconfigResult::Denied,
        ReconfigResult::ErrorWrongSsn,
        ReconfigResult::ErrorRequestAlreadyInProgress,
        ReconfigResult::ErrorBadSequenceNumber,
        ReconfigResult::InProgress,
        ReconfigResult::Unknown,
    ] {
        let mut a = Association::default();
        a.pending_reset_completions.insert(stream_id);
        insert_active_reset(&mut a, rsn, stream_id);

        let response: Box<dyn Param + Send + Sync> = Box::new(ParamReconfigResponse {
            reconfig_response_sequence_number: rsn,
            result,
        });
        a.handle_reconfig_param(&response, &mut vec![])?;

        let completed = matches!(
            a.poll(),
            Some(Event::Stream(StreamEvent::ResetComplete { id })) if id == stream_id
        );
        let should_complete = matches!(
            result,
            ReconfigResult::SuccessNop | ReconfigResult::SuccessPerformed
        );
        assert_eq!(completed, should_complete, "unexpected result for {result}");
    }

    Ok(())
}

#[test]
fn test_reconfig_retransmission_failure_is_terminal() {
    let rsn = 7;
    let stream_id = 1;
    let mut a = Association::default();
    a.pending_reset_completions.insert(stream_id);
    insert_active_reset(&mut a, rsn, stream_id);

    a.on_retransmission_failure(Timer::Reconfig);

    assert!(matches!(
        a.poll(),
        Some(Event::Stream(StreamEvent::ResetFailed {
            id,
            reason: StreamResetError::Failed,
        })) if id == stream_id
    ));
}

#[test]
fn test_ambiguous_reset_failure_keeps_existing_stream_quarantined() -> Result<()> {
    let stream_id = 1;
    let mut a = Association {
        state: AssociationState::Established,
        my_next_tsn: 1,
        ..Default::default()
    };
    assert!(
        a.create_stream(stream_id, false, PayloadProtocolIdentifier::Binary)
            .is_some()
    );

    a.stream(stream_id)?.stop()?;
    let _ = a.gather_outbound(Instant::now());
    assert!(a.active_reconfig.is_some());

    a.on_retransmission_failure(Timer::Reconfig);

    assert!(a.failed_reset_streams.contains(&stream_id));
    assert!(
        !a.stream(stream_id)?.is_writable(),
        "an unacknowledged reset may have succeeded at the peer, so old SSNs remain unsafe"
    );
    Ok(())
}

#[test]
fn test_denied_reset_emits_terminal_event_and_remains_quarantined() -> Result<()> {
    let rsn = 7;
    let stream_id = 1;
    let mut a = Association::default();
    a.pending_reset_completions.insert(stream_id);
    insert_active_reset(&mut a, rsn, stream_id);

    let response: Box<dyn Param + Send + Sync> = Box::new(ParamReconfigResponse {
        reconfig_response_sequence_number: rsn,
        result: ReconfigResult::Denied,
    });
    a.handle_reconfig_param(&response, &mut vec![])?;

    assert!(matches!(
        a.poll(),
        Some(Event::Stream(StreamEvent::ResetFailed {
            id,
            reason: StreamResetError::Denied,
        })) if id == stream_id
    ));
    assert!(!a.pending_reset_completions.contains(&stream_id));
    assert!(
        matches!(
            a.open_stream(stream_id, PayloadProtocolIdentifier::Binary),
            Err(Error::ErrStreamResetPending)
        ),
        "the quarantine must prevent the failed stream ID from being reused"
    );
    Ok(())
}

#[test]
fn test_reset_complete_preserves_stream_generations() {
    let stream_id = 1;
    let mut a = Association::default();

    assert!(
        a.create_stream(stream_id, false, PayloadProtocolIdentifier::Binary)
            .is_some()
    );
    a.unregister_stream(stream_id, true);

    // Incoming DATA can recreate an id while the previous reciprocal reset is
    // still pending. A second reset of that id is a distinct generation.
    assert!(a.get_or_create_stream(stream_id).is_some());
    a.unregister_stream(stream_id, true);

    a.emit_reset_complete([stream_id]);
    a.emit_reset_complete([stream_id]);

    let mut finished = 0;
    let mut reset_complete = 0;
    while let Some(event) = a.poll() {
        match event {
            Event::Stream(StreamEvent::Finished { id }) if id == stream_id => finished += 1,
            Event::Stream(StreamEvent::ResetComplete { id }) if id == stream_id => {
                reset_complete += 1;
            }
            _ => {}
        }
    }

    assert_eq!(finished, 2);
    assert_eq!(reset_complete, 2);
}

#[test]
fn test_reset_complete_preserves_generations_through_responses() -> Result<()> {
    let stream_id = 1;
    let first_rsn = 7;
    let second_rsn = 8;
    let mut a = Association::default();

    // Two Finished events for the same stream ID represent two distinct
    // incarnations. Each successful reset response must complete one.
    a.pending_reset_completions.insert(stream_id);
    a.pending_reset_completions.insert(stream_id);
    a.reconfigs
        .insert(first_rsn, outgoing_reset(first_rsn, stream_id));
    a.reconfigs
        .insert(second_rsn, outgoing_reset(second_rsn, stream_id));

    for rsn in [first_rsn, second_rsn] {
        a.active_reconfig = Some(rsn);
        let response: Box<dyn Param + Send + Sync> = Box::new(ParamReconfigResponse {
            reconfig_response_sequence_number: rsn,
            result: ReconfigResult::SuccessPerformed,
        });
        a.handle_reconfig_param(&response, &mut vec![])?;
    }

    let reset_complete = core::iter::from_fn(|| a.poll())
        .filter(|event| {
            matches!(
                event,
                Event::Stream(StreamEvent::ResetComplete { id }) if *id == stream_id
            )
        })
        .count();

    assert_eq!(
        reset_complete, 2,
        "each completed stream generation needs its own ResetComplete"
    );
    Ok(())
}

#[test]
fn test_overlapping_generations_success_then_denied_reports_success() -> Result<()> {
    let stream_id = 1;
    let mut a = Association::default();

    a.pending_reset_completions.insert(stream_id);
    a.pending_reset_completions.insert(stream_id);
    a.reconfigs.insert(7, outgoing_reset(7, stream_id));
    a.reconfigs.insert(8, outgoing_reset(8, stream_id));

    for (rsn, result) in [
        (7, ReconfigResult::SuccessPerformed),
        (8, ReconfigResult::Denied),
    ] {
        a.active_reconfig = Some(rsn);
        let response: Box<dyn Param + Send + Sync> = Box::new(ParamReconfigResponse {
            reconfig_response_sequence_number: rsn,
            result,
        });
        a.handle_reconfig_param(&response, &mut vec![])?;
    }

    let reset_complete = core::iter::from_fn(|| a.poll())
        .filter(|event| {
            matches!(
                event,
                Event::Stream(StreamEvent::ResetComplete { id }) if *id == stream_id
            )
        })
        .count();

    assert_eq!(
        reset_complete, 1,
        "the successful generation still needs its ResetComplete"
    );
    Ok(())
}

#[test]
fn test_overlapping_generations_denied_then_success_reports_only_success() -> Result<()> {
    let stream_id = 1;
    let mut a = Association::default();

    a.pending_reset_completions.insert(stream_id);
    a.pending_reset_completions.insert(stream_id);
    a.reconfigs.insert(7, outgoing_reset(7, stream_id));
    a.reconfigs.insert(8, outgoing_reset(8, stream_id));

    for (rsn, result) in [
        (7, ReconfigResult::Denied),
        (8, ReconfigResult::SuccessPerformed),
    ] {
        a.active_reconfig = Some(rsn);
        let response: Box<dyn Param + Send + Sync> = Box::new(ParamReconfigResponse {
            reconfig_response_sequence_number: rsn,
            result,
        });
        a.handle_reconfig_param(&response, &mut vec![])?;
    }

    let reset_complete = core::iter::from_fn(|| a.poll())
        .filter(|event| {
            matches!(
                event,
                Event::Stream(StreamEvent::ResetComplete { id }) if *id == stream_id
            )
        })
        .count();

    assert_eq!(
        reset_complete, 1,
        "the denied generation must not inherit a later successful completion"
    );
    Ok(())
}

#[test]
fn test_outgoing_reset_implicitly_acknowledges_pending_request() -> Result<()> {
    let local_rsn = 7;
    let stream_id = 1;
    let mut a = Association::default();

    insert_active_reset(&mut a, local_rsn, stream_id);
    a.timers
        .start(Timer::Reconfig, Instant::now(), a.rto_mgr.get_rto());

    // RFC 6525 section 5.2.2 E1: the response sequence number carried by an
    // incoming Outgoing Reset Request acknowledges our matching request.
    let request: Box<dyn Param + Send + Sync> = Box::new(ParamOutgoingResetRequest {
        reconfig_request_sequence_number: 8,
        reconfig_response_sequence_number: local_rsn,
        sender_last_tsn: a.peer_last_tsn,
        stream_identifiers: vec![],
    });
    a.handle_reconfig_param(&request, &mut vec![])?;

    assert!(
        !a.reconfigs.contains_key(&local_rsn),
        "the implicitly acknowledged request must no longer be in flight"
    );
    assert!(
        a.timers.get(Timer::Reconfig).is_none(),
        "the timer must stop after the final in-flight request is acknowledged"
    );
    Ok(())
}

#[test]
fn test_outgoing_reset_does_not_ack_unsent_reciprocal() -> Result<()> {
    let stream_id = 1;
    let mut a = Association {
        my_next_tsn: 1,
        ..Default::default()
    };
    assert!(
        a.create_stream(stream_id, false, PayloadProtocolIdentifier::Binary)
            .is_some()
    );

    // Handling this request queues a reciprocal Outgoing Reset Request in
    // `reply`, but it has not been transmitted and its timer is not running.
    let request: Box<dyn Param + Send + Sync> = Box::new(ParamOutgoingResetRequest {
        reconfig_request_sequence_number: 7,
        reconfig_response_sequence_number: u32::MAX,
        sender_last_tsn: a.peer_last_tsn,
        stream_identifiers: vec![stream_id],
    });
    let mut reply = vec![];
    a.handle_reconfig_param(&request, &mut reply)?;

    let reciprocal_rsn = *a.reconfigs.keys().next().unwrap();
    assert!(!reply.is_empty());
    assert!(a.timers.get(Timer::Reconfig).is_none());

    // RFC 6525 section 5.2.2 E1 only acknowledges a request for which the
    // Re-configuration Timer is running. A peer must not be able to pre-ack
    // the queued reciprocal before the application polls it for transmission.
    let premature_ack: Box<dyn Param + Send + Sync> = Box::new(ParamOutgoingResetRequest {
        reconfig_request_sequence_number: 8,
        reconfig_response_sequence_number: reciprocal_rsn,
        sender_last_tsn: a.peer_last_tsn,
        stream_identifiers: vec![],
    });
    a.handle_reconfig_param(&premature_ack, &mut vec![])?;

    let reset_complete = core::iter::from_fn(|| a.poll())
        .filter(|event| {
            matches!(
                event,
                Event::Stream(StreamEvent::ResetComplete { id }) if *id == stream_id
            )
        })
        .count();
    assert_eq!(
        (a.reconfigs.contains_key(&reciprocal_rsn), reset_complete),
        (true, 0),
        "an unsent reciprocal must remain pending and cannot complete a reset"
    );
    Ok(())
}

#[test]
fn test_reset_complete_does_not_override_newer_failure() -> Result<()> {
    let stream_id = 1;
    let mut a = Association::default();

    a.pending_reset_completions.insert(stream_id);
    a.pending_reset_completions.insert(stream_id);
    a.reconfigs.insert(7, outgoing_reset(7, stream_id));
    a.reconfigs.insert(8, outgoing_reset(8, stream_id));

    for (rsn, result) in [
        (7, ReconfigResult::SuccessPerformed),
        (8, ReconfigResult::Denied),
    ] {
        a.active_reconfig = Some(rsn);
        let response: Box<dyn Param + Send + Sync> = Box::new(ParamReconfigResponse {
            reconfig_response_sequence_number: rsn,
            result,
        });
        a.handle_reconfig_param(&response, &mut vec![])?;
    }

    assert!(matches!(
        a.poll(),
        Some(Event::Stream(StreamEvent::ResetComplete { id })) if id == stream_id
    ));
    assert!(matches!(
        a.poll(),
        Some(Event::Stream(StreamEvent::ResetFailed {
            id,
            reason: StreamResetError::Denied,
        })) if id == stream_id
    ));
    assert!(matches!(
        a.open_stream(stream_id, PayloadProtocolIdentifier::Binary),
        Err(Error::ErrStreamResetPending)
    ));
    Ok(())
}

#[test]
fn test_reconfig_response_does_not_ack_unsent_reciprocal() -> Result<()> {
    let stream_id = 1;
    let mut a = Association {
        my_next_tsn: 1,
        ..Default::default()
    };
    assert!(
        a.create_stream(stream_id, false, PayloadProtocolIdentifier::Binary)
            .is_some()
    );

    let request: Box<dyn Param + Send + Sync> = Box::new(ParamOutgoingResetRequest {
        reconfig_request_sequence_number: 7,
        reconfig_response_sequence_number: u32::MAX,
        sender_last_tsn: a.peer_last_tsn,
        stream_identifiers: vec![stream_id],
    });
    let mut reply = vec![];
    a.handle_reconfig_param(&request, &mut reply)?;

    let reciprocal_rsn = *a.reconfigs.keys().next().unwrap();
    assert!(!reply.is_empty());
    assert!(a.timers.get(Timer::Reconfig).is_none());

    // The reciprocal is only queued in the reply and has not been transmitted.
    // RFC 6525 H1 says to ignore a response for an RSN whose timer is not running.
    let premature_response: Box<dyn Param + Send + Sync> = Box::new(ParamReconfigResponse {
        reconfig_response_sequence_number: reciprocal_rsn,
        result: ReconfigResult::SuccessPerformed,
    });
    a.handle_reconfig_param(&premature_response, &mut vec![])?;

    let reset_complete = core::iter::from_fn(|| a.poll())
        .filter(|event| {
            matches!(
                event,
                Event::Stream(StreamEvent::ResetComplete { id }) if *id == stream_id
            )
        })
        .count();
    assert_eq!(
        (a.reconfigs.contains_key(&reciprocal_rsn), reset_complete),
        (true, 0),
        "RFC 6525 H1 requires a response for an RSN whose timer is not running to be ignored"
    );
    Ok(())
}

#[test]
fn test_outgoing_reset_does_not_ack_unsent_reciprocal_with_unrelated_timer() -> Result<()> {
    let stream_id = 1;
    let sent_rsn = 99;
    let mut a = Association {
        my_next_tsn: 1,
        ..Default::default()
    };
    assert!(
        a.create_stream(stream_id, false, PayloadProtocolIdentifier::Binary)
            .is_some()
    );

    insert_active_reset(&mut a, sent_rsn, 2);
    a.timers
        .start(Timer::Reconfig, Instant::now(), a.rto_mgr.get_rto());

    // The global timer belongs to the already-sent request above. Processing
    // this incoming request queues a distinct reciprocal, but does not send it.
    let request: Box<dyn Param + Send + Sync> = Box::new(ParamOutgoingResetRequest {
        reconfig_request_sequence_number: 7,
        reconfig_response_sequence_number: u32::MAX,
        sender_last_tsn: a.peer_last_tsn,
        stream_identifiers: vec![stream_id],
    });
    let mut reply = vec![];
    a.handle_reconfig_param(&request, &mut reply)?;

    let reciprocal_rsn = *a.reconfigs.keys().find(|&&rsn| rsn != sent_rsn).unwrap();
    assert!(!reply.is_empty());

    let premature_ack: Box<dyn Param + Send + Sync> = Box::new(ParamOutgoingResetRequest {
        reconfig_request_sequence_number: 8,
        reconfig_response_sequence_number: reciprocal_rsn,
        sender_last_tsn: a.peer_last_tsn,
        stream_identifiers: vec![3],
    });
    a.handle_reconfig_param(&premature_ack, &mut vec![])?;

    let reset_complete = core::iter::from_fn(|| a.poll())
        .filter(|event| {
            matches!(
                event,
                Event::Stream(StreamEvent::ResetComplete { id }) if *id == stream_id
            )
        })
        .count();
    assert_eq!(
        (a.reconfigs.contains_key(&reciprocal_rsn), reset_complete),
        (true, 0),
        "a timer running for another RSN must not make an unsent reciprocal acknowledgeable"
    );
    Ok(())
}

#[test]
fn test_failed_generation_stays_quarantined_after_other_success() -> Result<()> {
    let stream_id = 1;
    let mut a = Association::default();

    a.pending_reset_completions.insert(stream_id);
    a.pending_reset_completions.insert(stream_id);
    a.reconfigs.insert(7, outgoing_reset(7, stream_id));
    a.reconfigs.insert(8, outgoing_reset(8, stream_id));

    // The older generation completes, but the newer generation is denied.
    // A success for the former cannot make the latter safe to reuse.
    for (rsn, result) in [
        (7, ReconfigResult::SuccessPerformed),
        (8, ReconfigResult::Denied),
    ] {
        a.active_reconfig = Some(rsn);
        let response: Box<dyn Param + Send + Sync> = Box::new(ParamReconfigResponse {
            reconfig_response_sequence_number: rsn,
            result,
        });
        a.handle_reconfig_param(&response, &mut vec![])?;
    }

    assert!(matches!(
        a.open_stream(stream_id, PayloadProtocolIdentifier::Binary),
        Err(Error::ErrStreamResetPending)
    ));
    Ok(())
}

#[test]
fn test_second_reconfig_request_stays_buffered_while_timer_runs() -> Result<()> {
    let stream_id = 1;
    let sent_rsn = 99;
    let mut a = Association {
        state: AssociationState::Established,
        my_next_tsn: 1,
        ..Default::default()
    };
    assert!(
        a.create_stream(stream_id, false, PayloadProtocolIdentifier::Binary)
            .is_some()
    );

    insert_active_reset(&mut a, sent_rsn, 2);
    a.timers
        .start(Timer::Reconfig, Instant::now(), a.rto_mgr.get_rto());

    // Processing the peer's request creates a reciprocal request while another
    // local request is already in flight. RFC 6525 section 5.1.1 requires the
    // reciprocal request to remain buffered until the running timer stops.
    let request: Box<dyn Param + Send + Sync> = Box::new(ParamOutgoingResetRequest {
        reconfig_request_sequence_number: 7,
        reconfig_response_sequence_number: u32::MAX,
        sender_last_tsn: a.peer_last_tsn,
        stream_identifiers: vec![stream_id],
    });
    let mut reply = vec![];
    a.handle_reconfig_param(&request, &mut reply)?;

    let reciprocal_rsn = *a.reconfigs.keys().find(|&&rsn| rsn != sent_rsn).unwrap();
    a.control_queue.extend(reply);

    assert!(a.poll_transmit(Instant::now()).is_some());
    assert_eq!(
        a.active_reconfig,
        Some(sent_rsn),
        "a second request must remain buffered while the first request's timer runs"
    );
    assert!(a.reconfigs.contains_key(&reciprocal_rsn));
    assert_eq!(a.control_queue.len(), 1);

    let response: Box<dyn Param + Send + Sync> = Box::new(ParamReconfigResponse {
        reconfig_response_sequence_number: sent_rsn,
        result: ReconfigResult::SuccessPerformed,
    });
    a.handle_reconfig_param(&response, &mut vec![])?;
    assert!(a.poll_transmit(Instant::now()).is_some());
    assert_eq!(a.active_reconfig, Some(reciprocal_rsn));
    assert!(a.control_queue.is_empty());
    Ok(())
}

#[test]
fn test_peer_recreated_quarantined_stream_is_not_writable() -> Result<()> {
    let stream_id = 1;
    let mut a = Association::default();

    // The peer may start its new incoming generation before our reciprocal
    // outgoing reset is acknowledged. Reading that generation is safe, but
    // sending with a freshly initialized SSN is not yet safe.
    a.pending_reset_completions.insert(stream_id);
    insert_active_reset(&mut a, 7, stream_id);

    assert!(a.get_or_create_stream(stream_id).is_some());
    assert!(
        !a.stream(stream_id)?.is_writable(),
        "a pending reciprocal reset must quarantine the outgoing direction"
    );
    Ok(())
}

#[test]
fn test_pending_reset_blocks_existing_stream_writes() -> Result<()> {
    let stream_id = 1;
    let mut a = Association::default();
    assert!(
        a.create_stream(stream_id, false, PayloadProtocolIdentifier::Binary)
            .is_some()
    );
    insert_active_reset(&mut a, 7, stream_id);

    assert!(
        !a.stream(stream_id)?.is_writable(),
        "new SSNs must not be assigned while an outgoing reset is pending"
    );
    Ok(())
}

#[test]
fn test_reset_completion_does_not_reopen_finished_write_half() -> Result<()> {
    let stream_id = 1;
    let rsn = 7;
    let mut a = Association::default();
    assert!(
        a.create_stream(stream_id, false, PayloadProtocolIdentifier::Binary)
            .is_some()
    );
    insert_active_reset(&mut a, rsn, stream_id);

    // Finishing the write half is permanent, including while a reset request
    // temporarily makes the stream non-writable for protocol reasons.
    a.stream(stream_id)?.finish()?;

    let response: Box<dyn Param + Send + Sync> = Box::new(ParamReconfigResponse {
        reconfig_response_sequence_number: rsn,
        result: ReconfigResult::SuccessPerformed,
    });
    a.handle_reconfig_param(&response, &mut vec![])?;

    assert!(
        !a.stream(stream_id)?.is_writable(),
        "reset completion must not undo Stream::finish()"
    );
    Ok(())
}

#[test]
fn test_successful_outgoing_reset_restarts_stream_sequence_number() -> Result<()> {
    let stream_id = 1;
    let rsn = 7;
    let mut a = Association::default();
    assert!(
        a.create_stream(stream_id, false, PayloadProtocolIdentifier::Binary)
            .is_some()
    );
    a.streams.get_mut(&stream_id).unwrap().sequence_number = 9;
    insert_active_reset(&mut a, rsn, stream_id);

    let response: Box<dyn Param + Send + Sync> = Box::new(ParamReconfigResponse {
        reconfig_response_sequence_number: rsn,
        result: ReconfigResult::SuccessPerformed,
    });
    a.handle_reconfig_param(&response, &mut vec![])?;

    assert_eq!(
        a.streams.get(&stream_id).unwrap().sequence_number,
        0,
        "RFC 6525 section 5.2.7 H4 requires the affected outgoing SSN to reset"
    );
    Ok(())
}

#[test]
fn test_only_one_buffered_reconfig_is_sent_when_timer_is_idle() {
    let mut a = Association {
        state: AssociationState::Established,
        ..Default::default()
    };

    for (rsn, stream_id) in [(7, 1), (8, 2)] {
        insert_queued_reset(&mut a, rsn, stream_id);
    }
    assert!(a.timers.get(Timer::Reconfig).is_none());

    assert!(a.poll_transmit(Instant::now()).is_some());
    assert_eq!(a.active_reconfig, Some(7));
    assert_eq!(a.control_queue.len(), 1);
    assert!(a.reconfigs.contains_key(&8));

    let response: Box<dyn Param + Send + Sync> = Box::new(ParamReconfigResponse {
        reconfig_response_sequence_number: 7,
        result: ReconfigResult::SuccessPerformed,
    });
    a.handle_reconfig_param(&response, &mut vec![]).unwrap();
    assert!(a.poll_transmit(Instant::now()).is_some());
    assert_eq!(a.active_reconfig, Some(8));
    assert!(a.control_queue.is_empty());
}

#[test]
fn test_close_discards_buffered_reconfig_requests() -> Result<()> {
    let mut a = Association {
        state: AssociationState::Established,
        ..Default::default()
    };
    insert_active_reset(&mut a, 7, 1);
    insert_queued_reset(&mut a, 8, 2);
    a.timers
        .start(Timer::Reconfig, Instant::now(), a.rto_mgr.get_rto());

    a.close()?;

    assert!(
        a.poll_transmit(Instant::now()).is_none(),
        "a closed association must not send a buffered stream-reset request"
    );
    assert!(a.active_reconfig.is_none());
    assert!(a.reconfigs.is_empty());
    Ok(())
}

#[test]
fn test_in_progress_keeps_later_request_buffered() -> Result<()> {
    let mut a = Association {
        state: AssociationState::Established,
        ..Default::default()
    };

    insert_active_reset(&mut a, 7, 1);
    insert_queued_reset(&mut a, 8, 2);
    a.timers
        .start(Timer::Reconfig, Instant::now(), a.rto_mgr.get_rto());

    let response: Box<dyn Param + Send + Sync> = Box::new(ParamReconfigResponse {
        reconfig_response_sequence_number: 7,
        result: ReconfigResult::InProgress,
    });
    a.handle_reconfig_param(&response, &mut vec![])?;
    assert!(a.reconfigs.contains_key(&7));

    let _ = a.poll_transmit(Instant::now());
    assert_eq!(a.active_reconfig, Some(7));
    assert_eq!(a.control_queue.len(), 1);
    Ok(())
}

#[test]
fn test_local_reset_stays_queued_while_reconfig_timer_runs() -> Result<()> {
    let mut a = Association {
        state: AssociationState::Established,
        my_next_tsn: 1,
        ..Default::default()
    };

    insert_active_reset(&mut a, 7, 1);
    a.timers
        .start(Timer::Reconfig, Instant::now(), a.rto_mgr.get_rto());
    a.send_reset_request(2)?;

    let _ = a.poll_transmit(Instant::now());
    assert_eq!(
        a.reconfigs.len(),
        1,
        "a locally initiated reset must remain queued while another request is in flight"
    );
    assert_eq!(
        a.pending_reset_streams.iter().copied().collect::<Vec<_>>(),
        [2]
    );

    let response: Box<dyn Param + Send + Sync> = Box::new(ParamReconfigResponse {
        reconfig_response_sequence_number: 7,
        result: ReconfigResult::SuccessPerformed,
    });
    a.handle_reconfig_param(&response, &mut vec![])?;
    assert!(a.poll_transmit(Instant::now()).is_some());
    assert_ne!(a.active_reconfig, Some(7));
    assert!(a.active_reconfig.is_some());
    assert!(a.pending_reset_streams.is_empty());
    Ok(())
}

#[test]
fn test_local_reset_does_not_overtake_pending_data() -> Result<()> {
    let mut a = Association {
        state: AssociationState::Established,
        my_next_tsn: 1,
        ..Default::default()
    };
    queue_inflight(
        &mut a,
        ChunkPayloadData {
            tsn: 1,
            user_data: Bytes::from_static(b"inflight"),
            ..Default::default()
        },
    );
    queue_pending(
        &mut a,
        ChunkPayloadData {
            stream_identifier: 1,
            beginning_fragment: true,
            ending_fragment: true,
            user_data: Bytes::from_static(b"pending"),
            ..Default::default()
        },
    );
    a.send_reset_request(1)?;

    let _ = a.poll_transmit(Instant::now());
    assert!(a.active_reconfig.is_none());
    assert!(a.reconfigs.is_empty());
    assert_eq!(a.pending_reset_streams.len(), 1);
    Ok(())
}

#[test]
fn test_local_reset_is_not_blocked_by_unrelated_pending_data() -> Result<()> {
    let reset_stream_id = 1;
    let unrelated_stream_id = 2;
    let mut a = Association {
        state: AssociationState::Established,
        my_next_tsn: 2,
        cwnd: 0,
        rwnd: 0,
        mtu: 1400,
        ..Default::default()
    };
    queue_inflight(
        &mut a,
        ChunkPayloadData {
            tsn: 1,
            stream_identifier: unrelated_stream_id,
            user_data: Bytes::from_static(b"inflight"),
            ..Default::default()
        },
    );
    queue_pending(
        &mut a,
        ChunkPayloadData {
            stream_identifier: unrelated_stream_id,
            beginning_fragment: true,
            ending_fragment: true,
            user_data: Bytes::from_static(b"flow controlled"),
            ..Default::default()
        },
    );
    a.send_reset_request(reset_stream_id)?;

    let _ = a.gather_outbound(Instant::now());

    assert!(
        a.active_reconfig.is_some(),
        "flow-controlled DATA on another stream must not starve this reset"
    );
    assert!(
        a.outbound_queue.pending_len() != 0,
        "the test requires the unrelated DATA to remain flow controlled"
    );
    Ok(())
}

#[test]
fn test_reset_sender_last_tsn_wraps_at_zero() -> Result<()> {
    let stream_id = 1;
    let mut a = Association {
        state: AssociationState::Established,
        my_next_tsn: 0,
        ..Default::default()
    };
    a.send_reset_request(stream_id)?;

    let _ = a.gather_outbound(Instant::now());

    let reset = a
        .reconfigs
        .get(&a.active_reconfig.unwrap())
        .unwrap()
        .param_a
        .as_ref()
        .and_then(|param| param.as_any().downcast_ref::<ParamOutgoingResetRequest>())
        .unwrap();
    assert_eq!(
        reset.sender_last_tsn,
        u32::MAX,
        "the TSN preceding zero must wrap to u32::MAX"
    );
    Ok(())
}

#[test]
fn test_reset_request_sequence_number_wraps() -> Result<()> {
    let stream_id = 1;
    let mut a = Association {
        state: AssociationState::Established,
        my_next_tsn: 1,
        my_next_rsn: u32::MAX,
        ..Default::default()
    };
    a.send_reset_request(stream_id)?;

    let _ = a.gather_outbound(Instant::now());

    assert_eq!(a.active_reconfig, Some(u32::MAX));
    assert_eq!(
        a.my_next_rsn, 0,
        "the re-configuration request sequence number must wrap"
    );
    Ok(())
}

#[test]
fn test_local_reset_acknowledges_latest_peer_request_sequence() -> Result<()> {
    let peer_rsn = 41;
    let mut a = Association {
        state: AssociationState::Established,
        my_next_tsn: 1,
        ..Default::default()
    };

    let peer_request: Box<dyn Param + Send + Sync> = Box::new(ParamOutgoingResetRequest {
        reconfig_request_sequence_number: peer_rsn,
        reconfig_response_sequence_number: u32::MAX,
        sender_last_tsn: a.peer_last_tsn,
        stream_identifiers: vec![],
    });
    a.handle_reconfig_param(&peer_request, &mut vec![])?;
    assert_eq!(a.max_completed_reconfig_rsn, Some(peer_rsn));

    a.send_reset_request(1)?;
    let _ = a.gather_outbound(Instant::now());

    let reset = a
        .reconfigs
        .get(&a.active_reconfig.unwrap())
        .unwrap()
        .param_a
        .as_ref()
        .and_then(|param| param.as_any().downcast_ref::<ParamOutgoingResetRequest>())
        .unwrap();
    assert_eq!(
        reset.reconfig_response_sequence_number, peer_rsn,
        "RFC 6525 section 5.1.2 A4 requires the latest peer RSN in the response field"
    );
    Ok(())
}

#[test]
fn test_reciprocal_reset_covers_preexisting_pending_data() -> Result<()> {
    let stream_id = 1;
    let mut a = Association {
        state: AssociationState::Established,
        my_next_tsn: 1,
        cwnd: 1400,
        rwnd: 1400,
        mtu: 1400,
        ..Default::default()
    };
    assert!(
        a.create_stream(stream_id, false, PayloadProtocolIdentifier::Binary)
            .is_some()
    );
    queue_pending(
        &mut a,
        ChunkPayloadData {
            stream_identifier: stream_id,
            stream_sequence_number: 4,
            beginning_fragment: true,
            ending_fragment: true,
            user_data: Bytes::from_static(b"pending"),
            ..Default::default()
        },
    );

    // Receiving the peer's outgoing reset creates a reciprocal outgoing reset.
    // DATA accepted before that request must be assigned a TSN covered by the
    // reciprocal request's Sender's Last Assigned TSN boundary.
    let request: Box<dyn Param + Send + Sync> = Box::new(ParamOutgoingResetRequest {
        reconfig_request_sequence_number: 7,
        reconfig_response_sequence_number: u32::MAX,
        sender_last_tsn: a.peer_last_tsn,
        stream_identifiers: vec![stream_id],
    });
    let mut reply = vec![];
    a.handle_reconfig_param(&request, &mut reply)?;
    a.control_queue.extend(reply);

    let _ = a.gather_outbound(Instant::now());

    let data_tsn = a.outbound_queue.get(1).unwrap().tsn;
    let reciprocal = a
        .reconfigs
        .get(&a.active_reconfig.unwrap())
        .unwrap()
        .param_a
        .as_ref()
        .and_then(|param| param.as_any().downcast_ref::<ParamOutgoingResetRequest>())
        .unwrap();
    assert!(
        sna32gte(reciprocal.sender_last_tsn, data_tsn),
        "the reciprocal reset boundary must cover DATA queued before the reset"
    );
    Ok(())
}

#[test]
fn test_data_above_deferred_reset_boundary_waits_for_new_generation() -> Result<()> {
    let stream_id = 1;
    let mut a = Association {
        state: AssociationState::Established,
        peer_last_tsn: 0,
        my_next_tsn: 1,
        max_receive_buffer_size: 1024,
        max_receive_message_size: 1024,
        max_payload_size: 1200,
        ..Default::default()
    };
    assert!(
        a.create_stream(stream_id, false, PayloadProtocolIdentifier::Binary)
            .is_some()
    );

    let request: Box<dyn Param + Send + Sync> = Box::new(ParamOutgoingResetRequest {
        reconfig_request_sequence_number: 7,
        reconfig_response_sequence_number: u32::MAX,
        sender_last_tsn: 1,
        stream_identifiers: vec![stream_id],
    });
    a.handle_reconfig_param(&request, &mut vec![])?;
    assert!(a.reconfig_requests.contains_key(&7));

    // TSN 2 belongs to the post-reset generation, but arrives while TSN 1 is
    // still missing and the reset therefore remains InProgress.
    a.handle_data(&ChunkPayloadData {
        tsn: 2,
        stream_identifier: stream_id,
        stream_sequence_number: 0,
        beginning_fragment: true,
        ending_fragment: true,
        user_data: Bytes::from_static(b"new"),
        ..Default::default()
    })?;

    assert!(
        a.poll().is_none(),
        "post-reset DATA must not notify the old stream generation"
    );
    assert!(
        a.stream(stream_id)?.read()?.is_none(),
        "post-reset DATA must not be readable before the reset boundary arrives"
    );

    // Once TSN 1 arrives, its payload remains readable as the last message of
    // the old generation. Held TSN 2 is released only after that generation is
    // drained, into a newly created generation whose SSN starts at zero.
    a.handle_data(&ChunkPayloadData {
        tsn: 1,
        stream_identifier: stream_id,
        stream_sequence_number: 0,
        beginning_fragment: true,
        ending_fragment: true,
        user_data: Bytes::from_static(b"old"),
        ..Default::default()
    })?;

    let message = a.stream(stream_id)?.read()?.unwrap();
    let mut payload = [0; 3];
    assert_eq!(message.read(&mut payload)?, payload.len());
    assert_eq!(&payload, b"old");

    let message = a.stream(stream_id)?.read()?.unwrap();
    let mut payload = [0; 3];
    assert_eq!(message.read(&mut payload)?, payload.len());
    assert_eq!(&payload, b"new");
    Ok(())
}

#[test]
fn test_reset_boundary_data_remains_readable_before_finished() -> Result<()> {
    let stream_id = 1;
    let mut a = Association {
        state: AssociationState::Established,
        peer_last_tsn: 0,
        my_next_tsn: 1,
        max_receive_buffer_size: 1024,
        max_receive_message_size: 1024,
        max_payload_size: 1200,
        ..Default::default()
    };
    assert!(
        a.create_stream(stream_id, false, PayloadProtocolIdentifier::Binary)
            .is_some()
    );

    let request: Box<dyn Param + Send + Sync> = Box::new(ParamOutgoingResetRequest {
        reconfig_request_sequence_number: 7,
        reconfig_response_sequence_number: u32::MAX,
        sender_last_tsn: 1,
        stream_identifiers: vec![stream_id],
    });
    a.handle_reconfig_param(&request, &mut vec![])?;

    a.handle_data(&ChunkPayloadData {
        tsn: 1,
        stream_identifier: stream_id,
        stream_sequence_number: 0,
        beginning_fragment: true,
        ending_fragment: true,
        user_data: Bytes::from_static(b"last"),
        ..Default::default()
    })?;

    loop {
        match a.poll() {
            Some(Event::Stream(StreamEvent::Readable { id })) if id == stream_id => break,
            Some(Event::Stream(StreamEvent::Finished { id })) if id == stream_id => {
                panic!("Finished must not overtake readable boundary DATA")
            }
            Some(_) => {}
            None => panic!("missing Readable event for boundary DATA"),
        }
    }

    assert!(
        a.poll().is_none(),
        "Finished must remain hidden until the readable boundary DATA is drained"
    );

    let message = a.stream(stream_id)?.read()?.unwrap();
    let mut payload = [0; 4];
    assert_eq!(message.read(&mut payload)?, payload.len());
    assert_eq!(&payload, b"last");
    assert!(matches!(
        core::iter::from_fn(|| a.poll()).find(|event| matches!(
            event,
            Event::Stream(StreamEvent::Finished { id }) if *id == stream_id
        )),
        Some(Event::Stream(StreamEvent::Finished { id })) if id == stream_id
    ));
    Ok(())
}

#[test]
fn test_successive_resets_preserve_each_unread_generation() -> Result<()> {
    let stream_id = 1;
    let mut a = Association {
        state: AssociationState::Established,
        peer_last_tsn: 0,
        my_next_tsn: 1,
        max_receive_buffer_size: 1024,
        max_receive_message_size: 1024,
        max_payload_size: 1200,
        ..Default::default()
    };
    assert!(
        a.create_stream(stream_id, false, PayloadProtocolIdentifier::Binary)
            .is_some()
    );

    let first_reset: Box<dyn Param + Send + Sync> = Box::new(ParamOutgoingResetRequest {
        reconfig_request_sequence_number: 7,
        reconfig_response_sequence_number: u32::MAX,
        sender_last_tsn: 1,
        stream_identifiers: vec![stream_id],
    });
    a.handle_reconfig_param(&first_reset, &mut vec![])?;
    a.handle_data(&ChunkPayloadData {
        tsn: 1,
        stream_identifier: stream_id,
        stream_sequence_number: 0,
        beginning_fragment: true,
        ending_fragment: true,
        user_data: Bytes::from_static(b"gen-a"),
        ..Default::default()
    })?;

    // The successor arrives while generation A is still unread.
    a.handle_data(&ChunkPayloadData {
        tsn: 2,
        stream_identifier: stream_id,
        stream_sequence_number: 0,
        beginning_fragment: true,
        ending_fragment: true,
        user_data: Bytes::from_static(b"gen-b"),
        ..Default::default()
    })?;
    let second_reset: Box<dyn Param + Send + Sync> = Box::new(ParamOutgoingResetRequest {
        reconfig_request_sequence_number: 8,
        reconfig_response_sequence_number: u32::MAX,
        sender_last_tsn: 2,
        stream_identifiers: vec![stream_id],
    });
    a.handle_reconfig_param(&second_reset, &mut vec![])?;

    // Model successful reciprocal handshakes for both reset generations.
    a.emit_reset_complete([stream_id]);
    a.emit_reset_complete([stream_id]);

    let first = a.stream(stream_id)?.read()?.unwrap();
    let mut payload = [0; 5];
    assert_eq!(first.read(&mut payload)?, payload.len());
    assert_eq!(&payload, b"gen-a");

    let mut finished = 0;
    let mut reset_complete = 0;
    while reset_complete == 0 {
        match a.poll() {
            Some(Event::Stream(StreamEvent::Finished { id })) if id == stream_id => finished += 1,
            Some(Event::Stream(StreamEvent::ResetComplete { id })) if id == stream_id => {
                reset_complete += 1;
            }
            Some(_) => {}
            None => panic!("generation A terminal event was blocked by unread generation B"),
        }
    }
    assert_eq!(finished, 1);

    assert!(
        core::iter::from_fn(|| a.poll()).any(|event| matches!(
            event,
            Event::Stream(StreamEvent::Readable { id }) if id == stream_id
        )),
        "generation B must advertise readability before its Finished event"
    );

    let second = a.stream(stream_id)?.read()?.unwrap();
    let mut payload = [0; 5];
    assert_eq!(second.read(&mut payload)?, payload.len());
    assert_eq!(&payload, b"gen-b");
    assert!(
        a.stream(stream_id).is_err(),
        "generation B was also reset and must retire after it is drained"
    );

    while let Some(event) = a.poll() {
        match event {
            Event::Stream(StreamEvent::Finished { id }) if id == stream_id => finished += 1,
            Event::Stream(StreamEvent::ResetComplete { id }) if id == stream_id => {
                reset_complete += 1;
            }
            _ => {}
        }
    }
    assert_eq!(finished, 2);
    assert_eq!(reset_complete, 2);
    Ok(())
}

fn association_with_retiring_boundary_data() -> Result<Association> {
    let stream_id = 1;
    let mut a = Association {
        state: AssociationState::Established,
        peer_last_tsn: 0,
        my_next_tsn: 1,
        max_receive_buffer_size: 1024,
        max_receive_message_size: 1024,
        max_payload_size: 1200,
        use_forward_tsn: true,
        ..Default::default()
    };
    assert!(
        a.create_stream(stream_id, false, PayloadProtocolIdentifier::Binary)
            .is_some()
    );
    let reset: Box<dyn Param + Send + Sync> = Box::new(ParamOutgoingResetRequest {
        reconfig_request_sequence_number: 7,
        reconfig_response_sequence_number: u32::MAX,
        sender_last_tsn: 1,
        stream_identifiers: vec![stream_id],
    });
    a.handle_reconfig_param(&reset, &mut vec![])?;
    a.handle_data(&ChunkPayloadData {
        tsn: 1,
        stream_identifier: stream_id,
        stream_sequence_number: 0,
        beginning_fragment: true,
        ending_fragment: true,
        user_data: Bytes::from_static(b"old"),
        ..Default::default()
    })?;
    Ok(a)
}

fn assert_successor_ssn_one_is_readable(mut a: Association) -> Result<()> {
    let stream_id = 1;
    let old = a.stream(stream_id)?.read()?.unwrap();
    let mut payload = [0; 3];
    assert_eq!(old.read(&mut payload)?, payload.len());
    assert_eq!(&payload, b"old");

    a.handle_data(&ChunkPayloadData {
        tsn: 3,
        stream_identifier: stream_id,
        stream_sequence_number: 1,
        beginning_fragment: true,
        ending_fragment: true,
        user_data: Bytes::from_static(b"new"),
        ..Default::default()
    })?;
    let successor = a.stream(stream_id)?.read()?.unwrap();
    let mut payload = [0; 3];
    assert_eq!(successor.read(&mut payload)?, payload.len());
    assert_eq!(&payload, b"new");
    Ok(())
}

#[test]
fn test_forward_tsn_skip_is_applied_to_reset_successor() -> Result<()> {
    let mut a = association_with_retiring_boundary_data()?;
    a.handle_forward_tsn(&ChunkForwardTsn {
        new_cumulative_tsn: 2,
        streams: vec![ChunkForwardTsnStream {
            identifier: 1,
            sequence: 0,
        }],
    })?;
    assert_successor_ssn_one_is_readable(a)
}

#[test]
fn test_i_forward_tsn_skip_is_applied_to_reset_successor() -> Result<()> {
    let mut a = association_with_retiring_boundary_data()?;
    a.handle_i_forward_tsn(&ChunkIForwardTsn {
        new_cumulative_tsn: 2,
        streams: vec![ChunkIForwardTsnStream {
            identifier: 1,
            unordered: false,
            mid: 0,
        }],
    })?;
    assert_successor_ssn_one_is_readable(a)
}

#[test]
fn test_forward_tsn_during_in_progress_reset_applies_to_old_generation() -> Result<()> {
    let stream_id = 1;
    let mut a = Association {
        state: AssociationState::Established,
        peer_last_tsn: 0,
        my_next_tsn: 1,
        max_receive_buffer_size: 1024,
        max_receive_message_size: 1024,
        max_payload_size: 1200,
        use_forward_tsn: true,
        ..Default::default()
    };
    assert!(
        a.create_stream(stream_id, false, PayloadProtocolIdentifier::Binary)
            .is_some()
    );
    let reset: Box<dyn Param + Send + Sync> = Box::new(ParamOutgoingResetRequest {
        reconfig_request_sequence_number: 7,
        reconfig_response_sequence_number: u32::MAX,
        sender_last_tsn: 1,
        stream_identifiers: vec![stream_id],
    });
    a.handle_reconfig_param(&reset, &mut vec![])?;

    // The reset has not reached its TSN boundary yet, so this skip belongs to
    // the old generation even though the association-level cumulative TSN
    // advances beyond that boundary.
    a.handle_forward_tsn(&ChunkForwardTsn {
        new_cumulative_tsn: 2,
        streams: vec![ChunkForwardTsnStream {
            identifier: stream_id,
            sequence: 0,
        }],
    })?;

    a.handle_data(&ChunkPayloadData {
        tsn: 3,
        stream_identifier: stream_id,
        stream_sequence_number: 0,
        beginning_fragment: true,
        ending_fragment: true,
        user_data: Bytes::from_static(b"new"),
        ..Default::default()
    })?;
    let successor = a.stream(stream_id)?.read()?.unwrap();
    let mut payload = [0; 3];
    assert_eq!(successor.read(&mut payload)?, payload.len());
    assert_eq!(&payload, b"new");
    Ok(())
}

fn association_with_queued_second_reset() -> Result<Association> {
    let mut a = association_with_retiring_boundary_data()?;
    let second_reset: Box<dyn Param + Send + Sync> = Box::new(ParamOutgoingResetRequest {
        reconfig_request_sequence_number: 8,
        reconfig_response_sequence_number: u32::MAX,
        sender_last_tsn: 2,
        stream_identifiers: vec![1],
    });
    a.handle_reconfig_param(&second_reset, &mut vec![])?;
    assert!(a.reconfig_requests.contains_key(&8));
    Ok(a)
}

fn assert_generation_c_ssn_zero_is_readable(mut a: Association) -> Result<()> {
    let old = a.stream(1)?.read()?.unwrap();
    let mut payload = [0; 3];
    assert_eq!(old.read(&mut payload)?, payload.len());
    assert_eq!(&payload, b"old");

    a.handle_data(&ChunkPayloadData {
        tsn: 4,
        stream_identifier: 1,
        stream_sequence_number: 0,
        beginning_fragment: true,
        ending_fragment: true,
        user_data: Bytes::from_static(b"gen-c"),
        ..Default::default()
    })?;
    let generation_c = a.stream(1)?.read()?.unwrap();
    let mut payload = [0; 5];
    assert_eq!(generation_c.read(&mut payload)?, payload.len());
    assert_eq!(&payload, b"gen-c");
    Ok(())
}

#[test]
fn test_forward_tsn_skip_is_scoped_to_queued_reset_generation() -> Result<()> {
    let mut a = association_with_queued_second_reset()?;
    a.handle_forward_tsn(&ChunkForwardTsn {
        // TSN 2 is generation B's abandoned SSN 0. TSN 3 belongs to an
        // unrelated stream, so the aggregate cumulative point cannot identify
        // which stream generation the per-stream SSN describes.
        new_cumulative_tsn: 3,
        streams: vec![ChunkForwardTsnStream {
            identifier: 1,
            sequence: 0,
        }],
    })?;
    assert_generation_c_ssn_zero_is_readable(a)
}

#[test]
fn test_retransmitted_forward_tsn_stays_with_reset_generation() -> Result<()> {
    let mut a = association_with_queued_second_reset()?;

    // The first FORWARD-TSN advances through generation B's abandoned SSN 0
    // and lets its pending reset complete.
    a.handle_forward_tsn(&ChunkForwardTsn {
        new_cumulative_tsn: 2,
        streams: vec![ChunkForwardTsnStream {
            identifier: 1,
            sequence: 0,
        }],
    })?;
    assert!(!a.reconfig_requests.contains_key(&8));

    // If the resulting SACK is lost, the sender can legitimately repeat the
    // same stream skip while advancing over an unrelated abandoned TSN. The
    // repeated entry still belongs to generation B, not the future tail.
    a.handle_forward_tsn(&ChunkForwardTsn {
        new_cumulative_tsn: 3,
        streams: vec![ChunkForwardTsnStream {
            identifier: 1,
            sequence: 0,
        }],
    })?;

    assert_generation_c_ssn_zero_is_readable(a)
}

fn assert_successor_ssn_one_after_reused_forward_ssn(mut a: Association) -> Result<()> {
    let old = a.stream(1)?.read()?.unwrap();
    let mut payload = [0; 3];
    assert_eq!(old.read(&mut payload)?, payload.len());
    assert_eq!(&payload, b"old");

    a.handle_data(&ChunkPayloadData {
        tsn: 4,
        stream_identifier: 1,
        stream_sequence_number: 1,
        beginning_fragment: true,
        ending_fragment: true,
        user_data: Bytes::from_static(b"gen-c"),
        ..Default::default()
    })?;
    let generation_c = a
        .stream(1)?
        .read()?
        .expect("generation C SSN 1 should follow its abandoned SSN 0");
    let mut payload = [0; 5];
    assert_eq!(generation_c.read(&mut payload)?, payload.len());
    assert_eq!(&payload, b"gen-c");
    Ok(())
}

#[test]
fn test_successor_forward_tsn_can_reuse_reset_ssn() -> Result<()> {
    let mut a = association_with_queued_second_reset()?;
    for new_cumulative_tsn in [2, 3] {
        a.handle_forward_tsn(&ChunkForwardTsn {
            new_cumulative_tsn,
            streams: vec![ChunkForwardTsnStream {
                identifier: 1,
                sequence: 0,
            }],
        })?;
    }
    assert_successor_ssn_one_after_reused_forward_ssn(a)
}

#[test]
fn test_successor_i_forward_tsn_can_reuse_reset_mid() -> Result<()> {
    let mut a = association_with_queued_second_reset()?;
    for new_cumulative_tsn in [2, 3] {
        a.handle_i_forward_tsn(&ChunkIForwardTsn {
            new_cumulative_tsn,
            streams: vec![ChunkIForwardTsnStream {
                identifier: 1,
                unordered: false,
                mid: 0,
            }],
        })?;
    }
    assert_successor_ssn_one_after_reused_forward_ssn(a)
}

#[test]
fn test_repeated_old_forward_tsn_does_not_skip_later_successor_ssn() -> Result<()> {
    let mut a = association_with_queued_second_reset()?;
    for new_cumulative_tsn in [2, 3] {
        a.handle_forward_tsn(&ChunkForwardTsn {
            new_cumulative_tsn,
            streams: vec![ChunkForwardTsnStream {
                identifier: 1,
                sequence: 1,
            }],
        })?;
    }

    let old = a.stream(1)?.read()?.unwrap();
    let mut payload = [0; 3];
    assert_eq!(old.read(&mut payload)?, payload.len());

    for (tsn, ssn, bytes) in [(4, 0, b"zero".as_slice()), (5, 1, b"one".as_slice())] {
        a.handle_data(&ChunkPayloadData {
            tsn,
            stream_identifier: 1,
            stream_sequence_number: ssn,
            beginning_fragment: true,
            ending_fragment: true,
            user_data: Bytes::copy_from_slice(bytes),
            ..Default::default()
        })?;
        let message = a
            .stream(1)?
            .read()?
            .expect("an old-generation repeat must not skip a live successor SSN");
        let mut payload = [0; 4];
        assert_eq!(message.read(&mut payload)?, bytes.len());
        assert_eq!(&payload[..bytes.len()], bytes);
    }
    Ok(())
}

#[test]
fn test_repeated_old_forward_tsn_preserves_partial_successor_above_cumulative() -> Result<()> {
    let mut a = association_with_queued_second_reset()?;
    a.handle_forward_tsn(&ChunkForwardTsn {
        new_cumulative_tsn: 2,
        streams: vec![ChunkForwardTsnStream {
            identifier: 1,
            sequence: 0,
        }],
    })?;
    a.handle_data(&ChunkPayloadData {
        tsn: 4,
        stream_identifier: 1,
        stream_sequence_number: 0,
        beginning_fragment: true,
        ending_fragment: false,
        user_data: Bytes::from_static(b"live-"),
        ..Default::default()
    })?;

    // TSN 3 is unrelated to this stream. Repeating generation B's skip must
    // not discard generation C's fragment at TSN 4.
    a.handle_forward_tsn(&ChunkForwardTsn {
        new_cumulative_tsn: 3,
        streams: vec![ChunkForwardTsnStream {
            identifier: 1,
            sequence: 0,
        }],
    })?;

    let old = a.stream(1)?.read()?.unwrap();
    let mut payload = [0; 3];
    assert_eq!(old.read(&mut payload)?, payload.len());
    a.handle_data(&ChunkPayloadData {
        tsn: 5,
        stream_identifier: 1,
        stream_sequence_number: 0,
        beginning_fragment: false,
        ending_fragment: true,
        user_data: Bytes::from_static(b"data"),
        ..Default::default()
    })?;

    let message = a
        .stream(1)?
        .read()?
        .expect("the successor fragments should still reassemble");
    let mut payload = [0; 9];
    assert_eq!(message.read(&mut payload)?, payload.len());
    assert_eq!(&payload, b"live-data");
    Ok(())
}

#[test]
fn test_deferred_forward_tsn_discards_partial_missing_pre_cumulative_tsn() -> Result<()> {
    let mut a = association_with_retiring_boundary_data()?;
    let reset: Box<dyn Param + Send + Sync> = Box::new(ParamOutgoingResetRequest {
        reconfig_request_sequence_number: 8,
        reconfig_response_sequence_number: u32::MAX,
        sender_last_tsn: 4,
        stream_identifiers: vec![1],
    });
    a.handle_reconfig_param(&reset, &mut vec![])?;

    // B/SSN 0 is missing its beginning at TSN 2, while B/SSN 1 is complete.
    for chunk in [
        ChunkPayloadData {
            tsn: 3,
            stream_identifier: 1,
            stream_sequence_number: 0,
            beginning_fragment: false,
            ending_fragment: true,
            user_data: Bytes::from_static(b"orphan"),
            ..Default::default()
        },
        ChunkPayloadData {
            tsn: 4,
            stream_identifier: 1,
            stream_sequence_number: 1,
            beginning_fragment: true,
            ending_fragment: true,
            user_data: Bytes::from_static(b"next"),
            ..Default::default()
        },
    ] {
        a.handle_data(&chunk)?;
    }
    a.handle_forward_tsn(&ChunkForwardTsn {
        new_cumulative_tsn: 2,
        streams: vec![ChunkForwardTsnStream {
            identifier: 1,
            sequence: 0,
        }],
    })?;

    assert_eq!(
        a.get_my_receiver_window_credit(),
        a.max_receive_buffer_size - 3 - 4,
        "the partial message missing TSN 2 must release its six bytes immediately"
    );
    let old = a.stream(1)?.read()?.unwrap();
    let mut payload = [0; 3];
    assert_eq!(old.read(&mut payload)?, payload.len());

    let next = a
        .stream(1)?
        .read()?
        .expect("discarding abandoned SSN 0 must unblock complete SSN 1");
    let mut payload = [0; 4];
    assert_eq!(next.read(&mut payload)?, payload.len());
    assert_eq!(&payload, b"next");
    Ok(())
}

#[test]
fn test_reordered_successor_waits_before_consuming_old_forward_tsn() -> Result<()> {
    let mut a = association_with_queued_second_reset()?;
    for new_cumulative_tsn in [2, 3] {
        a.handle_forward_tsn(&ChunkForwardTsn {
            new_cumulative_tsn,
            streams: vec![ChunkForwardTsnStream {
                identifier: 1,
                sequence: 0,
            }],
        })?;
    }
    a.handle_data(&ChunkPayloadData {
        tsn: 5,
        stream_identifier: 1,
        stream_sequence_number: 1,
        beginning_fragment: true,
        ending_fragment: true,
        user_data: Bytes::from_static(b"one"),
        ..Default::default()
    })?;

    let old = a.stream(1)?.read()?.unwrap();
    let mut payload = [0; 3];
    assert_eq!(old.read(&mut payload)?, payload.len());
    assert!(
        a.stream(1)?.read()?.is_none(),
        "SSN 1 must wait while SSN 0's post-FWD TSN is still missing"
    );

    a.handle_data(&ChunkPayloadData {
        tsn: 4,
        stream_identifier: 1,
        stream_sequence_number: 0,
        beginning_fragment: true,
        ending_fragment: true,
        user_data: Bytes::from_static(b"zero"),
        ..Default::default()
    })?;
    for expected in [b"zero".as_slice(), b"one".as_slice()] {
        let message = a
            .stream(1)?
            .read()?
            .expect("successor messages should become readable in SSN order");
        let mut payload = [0; 4];
        assert_eq!(message.read(&mut payload)?, expected.len());
        assert_eq!(&payload[..expected.len()], expected);
    }
    Ok(())
}

#[test]
fn test_reordered_successor_waits_before_consuming_repeated_old_forward_tsn() -> Result<()> {
    let mut a = association_with_queued_second_reset()?;
    for new_cumulative_tsn in [2, 3] {
        a.handle_forward_tsn(&ChunkForwardTsn {
            new_cumulative_tsn,
            streams: vec![ChunkForwardTsnStream {
                identifier: 1,
                sequence: 1,
            }],
        })?;
    }
    a.handle_data(&ChunkPayloadData {
        tsn: 5,
        stream_identifier: 1,
        stream_sequence_number: 1,
        beginning_fragment: true,
        ending_fragment: true,
        user_data: Bytes::from_static(b"one"),
        ..Default::default()
    })?;

    let old = a.stream(1)?.read()?.unwrap();
    let mut payload = [0; 3];
    assert_eq!(old.read(&mut payload)?, payload.len());
    assert!(
        a.stream(1)?.read()?.is_none(),
        "SSN 1 must wait while SSN 0's post-FWD TSN is still missing"
    );

    a.handle_data(&ChunkPayloadData {
        tsn: 4,
        stream_identifier: 1,
        stream_sequence_number: 0,
        beginning_fragment: true,
        ending_fragment: true,
        user_data: Bytes::from_static(b"zero"),
        ..Default::default()
    })?;
    for expected in [b"zero".as_slice(), b"one".as_slice()] {
        let message = a
            .stream(1)?
            .read()?
            .expect("successor messages should become readable in SSN order");
        let mut payload = [0; 4];
        assert_eq!(message.read(&mut payload)?, expected.len());
        assert_eq!(&payload[..expected.len()], expected);
    }
    Ok(())
}

#[test]
fn test_successor_forward_tsn_releases_covered_out_of_order_message() -> Result<()> {
    let mut a = association_with_queued_second_reset()?;
    a.handle_forward_tsn(&ChunkForwardTsn {
        new_cumulative_tsn: 2,
        streams: vec![ChunkForwardTsnStream {
            identifier: 1,
            sequence: 0,
        }],
    })?;
    a.handle_data(&ChunkPayloadData {
        tsn: 5,
        stream_identifier: 1,
        stream_sequence_number: 1,
        beginning_fragment: true,
        ending_fragment: true,
        user_data: Bytes::from_static(b"one"),
        ..Default::default()
    })?;

    // The sender did not receive the gap acknowledgement for TSN 5 and
    // abandons both missing SSN 0 and the already-complete SSN 1.
    a.handle_forward_tsn(&ChunkForwardTsn {
        new_cumulative_tsn: 5,
        streams: vec![ChunkForwardTsnStream {
            identifier: 1,
            sequence: 1,
        }],
    })?;

    let old = a.stream(1)?.read()?.unwrap();
    let mut payload = [0; 3];
    assert_eq!(old.read(&mut payload)?, payload.len());

    let successor = a
        .stream(1)?
        .read()?
        .expect("the successor skip must release the complete covered message");
    let mut payload = [0; 3];
    assert_eq!(successor.read(&mut payload)?, payload.len());
    assert_eq!(&payload, b"one");
    Ok(())
}

#[test]
fn test_cross_generation_forward_tsn_does_not_skip_uncovered_successor_ssn() -> Result<()> {
    let mut a = association_with_queued_second_reset()?;
    a.handle_forward_tsn(&ChunkForwardTsn {
        new_cumulative_tsn: 2,
        streams: vec![ChunkForwardTsnStream {
            identifier: 1,
            sequence: 1,
        }],
    })?;
    a.handle_data(&ChunkPayloadData {
        tsn: 4,
        stream_identifier: 1,
        stream_sequence_number: 0,
        beginning_fragment: true,
        ending_fragment: true,
        user_data: Bytes::from_static(b"zero"),
        ..Default::default()
    })?;

    // A lost SACK can make one FORWARD-TSN span old SSN 1 and successor SSN
    // 0. The old maximum SSN must not also skip successor SSN 1.
    a.handle_forward_tsn(&ChunkForwardTsn {
        new_cumulative_tsn: 4,
        streams: vec![ChunkForwardTsnStream {
            identifier: 1,
            sequence: 1,
        }],
    })?;

    let old = a.stream(1)?.read()?.unwrap();
    let mut payload = [0; 3];
    assert_eq!(old.read(&mut payload)?, payload.len());
    let zero = a.stream(1)?.read()?.unwrap();
    let mut payload = [0; 4];
    assert_eq!(zero.read(&mut payload)?, payload.len());
    assert_eq!(&payload, b"zero");

    a.handle_data(&ChunkPayloadData {
        tsn: 5,
        stream_identifier: 1,
        stream_sequence_number: 1,
        beginning_fragment: true,
        ending_fragment: true,
        user_data: Bytes::from_static(b"one"),
        ..Default::default()
    })?;
    let one = a
        .stream(1)?
        .read()?
        .expect("the successor SSN outside the covered TSN range must remain live");
    let mut payload = [0; 3];
    assert_eq!(one.read(&mut payload)?, payload.len());
    assert_eq!(&payload, b"one");
    Ok(())
}

#[test]
fn test_successor_forward_tsn_releases_covered_message_before_lost_tail() -> Result<()> {
    let mut a = association_with_queued_second_reset()?;
    a.handle_forward_tsn(&ChunkForwardTsn {
        new_cumulative_tsn: 2,
        streams: vec![ChunkForwardTsnStream {
            identifier: 1,
            sequence: 0,
        }],
    })?;
    a.handle_data(&ChunkPayloadData {
        tsn: 4,
        stream_identifier: 1,
        stream_sequence_number: 1,
        beginning_fragment: true,
        ending_fragment: true,
        user_data: Bytes::from_static(b"one"),
        ..Default::default()
    })?;

    // Successor SSN 0/TSN 3 and SSN 2/TSN 5 were both abandoned. The
    // complete covered SSN 1 can be released before SSN 2 is observed.
    a.handle_forward_tsn(&ChunkForwardTsn {
        new_cumulative_tsn: 5,
        streams: vec![ChunkForwardTsnStream {
            identifier: 1,
            sequence: 2,
        }],
    })?;

    let old = a.stream(1)?.read()?.unwrap();
    let mut payload = [0; 3];
    assert_eq!(old.read(&mut payload)?, payload.len());
    let one = a
        .stream(1)?
        .read()?
        .expect("covered successor data must not wait for the lost final SSN");
    let mut payload = [0; 3];
    assert_eq!(one.read(&mut payload)?, payload.len());
    assert_eq!(&payload, b"one");
    Ok(())
}

#[test]
fn test_resolved_successor_prefix_releases_message_before_stale_tail() -> Result<()> {
    let mut a = association_with_queued_second_reset()?;
    a.handle_forward_tsn(&ChunkForwardTsn {
        new_cumulative_tsn: 2,
        streams: vec![ChunkForwardTsnStream {
            identifier: 1,
            sequence: 2,
        }],
    })?;
    a.handle_forward_tsn(&ChunkForwardTsn {
        new_cumulative_tsn: 3,
        streams: vec![ChunkForwardTsnStream {
            identifier: 1,
            sequence: 2,
        }],
    })?;
    a.handle_data(&ChunkPayloadData {
        tsn: 4,
        stream_identifier: 1,
        stream_sequence_number: 1,
        beginning_fragment: true,
        ending_fragment: true,
        user_data: Bytes::from_static(b"one"),
        ..Default::default()
    })?;

    let old = a.stream(1)?.read()?.unwrap();
    let mut payload = [0; 3];
    assert_eq!(old.read(&mut payload)?, payload.len());
    let one = a
        .stream(1)?
        .read()?
        .expect("resolved successor data must not wait for the stale old tail");
    let mut payload = [0; 3];
    assert_eq!(one.read(&mut payload)?, payload.len());
    assert_eq!(&payload, b"one");
    Ok(())
}

fn assert_covered_partial_after_unaffected_anchor_is_discarded(last_ssn: u16) -> Result<()> {
    let mut a = association_with_queued_second_reset()?;
    for new_cumulative_tsn in [2, 3] {
        a.handle_forward_tsn(&ChunkForwardTsn {
            new_cumulative_tsn,
            streams: vec![ChunkForwardTsnStream {
                identifier: 1,
                sequence: last_ssn,
            }],
        })?;
    }
    for chunk in [
        ChunkPayloadData {
            tsn: 4,
            stream_identifier: 1,
            stream_sequence_number: 1,
            beginning_fragment: false,
            ending_fragment: true,
            user_data: Bytes::from_static(b"orphan"),
            ..Default::default()
        },
        ChunkPayloadData {
            tsn: 5,
            stream_identifier: 1,
            stream_sequence_number: 0,
            beginning_fragment: true,
            ending_fragment: true,
            user_data: Bytes::from_static(b"zero"),
            ..Default::default()
        },
    ] {
        a.handle_data(&chunk)?;
    }

    let old = a.stream(1)?.read()?.unwrap();
    let mut payload = [0; 3];
    assert_eq!(old.read(&mut payload)?, payload.len());
    let zero = a.stream(1)?.read()?.unwrap();
    let mut payload = [0; 4];
    assert_eq!(zero.read(&mut payload)?, payload.len());
    assert_eq!(&payload, b"zero");
    assert_eq!(
        a.get_my_receiver_window_credit(),
        a.max_receive_buffer_size,
        "the covered partial message must release its receive-window credit"
    );
    Ok(())
}

#[test]
fn test_full_forward_update_discards_covered_partial_after_anchor() -> Result<()> {
    assert_covered_partial_after_unaffected_anchor_is_discarded(1)
}

#[test]
fn test_partial_forward_update_discards_covered_partial_after_anchor() -> Result<()> {
    assert_covered_partial_after_unaffected_anchor_is_discarded(2)
}

#[test]
fn test_drained_generation_discards_retained_forward_suffix() -> Result<()> {
    let mut a = association_with_retiring_boundary_data()?;
    let reset: Box<dyn Param + Send + Sync> = Box::new(ParamOutgoingResetRequest {
        reconfig_request_sequence_number: 8,
        reconfig_response_sequence_number: u32::MAX,
        sender_last_tsn: 5,
        stream_identifiers: vec![1],
    });
    a.handle_reconfig_param(&reset, &mut vec![])?;
    a.handle_data(&ChunkPayloadData {
        tsn: 4,
        stream_identifier: 1,
        stream_sequence_number: 1,
        beginning_fragment: true,
        ending_fragment: true,
        user_data: Bytes::from_static(b"one"),
        ..Default::default()
    })?;
    a.handle_forward_tsn(&ChunkForwardTsn {
        new_cumulative_tsn: 5,
        streams: vec![ChunkForwardTsnStream {
            identifier: 1,
            sequence: 2,
        }],
    })?;

    let old = a.stream(1)?.read()?.unwrap();
    let mut payload = [0; 3];
    assert_eq!(old.read(&mut payload)?, payload.len());
    let one = a.stream(1)?.read()?.unwrap();
    let mut payload = [0; 3];
    assert_eq!(one.read(&mut payload)?, payload.len());
    assert_eq!(&payload, b"one");
    assert!(
        a.deferred_forward_tsns.get(&1).is_none_or(|updates| {
            updates
                .iter()
                .all(|update| update.generation_boundary != Some(5))
        }),
        "a drained reset generation must discard its retained ambiguous suffix"
    );
    Ok(())
}

#[test]
fn test_first_reset_discards_retained_tail_forward_suffix() -> Result<()> {
    let mut a = association_with_queued_second_reset()?;
    a.handle_forward_tsn(&ChunkForwardTsn {
        new_cumulative_tsn: 2,
        streams: vec![ChunkForwardTsnStream {
            identifier: 1,
            sequence: 0,
        }],
    })?;
    a.handle_data(&ChunkPayloadData {
        tsn: 4,
        stream_identifier: 1,
        stream_sequence_number: 1,
        beginning_fragment: true,
        ending_fragment: true,
        user_data: Bytes::from_static(b"one"),
        ..Default::default()
    })?;
    a.handle_forward_tsn(&ChunkForwardTsn {
        new_cumulative_tsn: 5,
        streams: vec![ChunkForwardTsnStream {
            identifier: 1,
            sequence: 2,
        }],
    })?;

    let old = a.stream(1)?.read()?.unwrap();
    let mut payload = [0; 3];
    assert_eq!(old.read(&mut payload)?, payload.len());
    let one = a.stream(1)?.read()?.unwrap();
    let mut payload = [0; 3];
    assert_eq!(one.read(&mut payload)?, payload.len());
    assert_eq!(&payload, b"one");
    assert!(
        a.deferred_forward_tsns[&1]
            .iter()
            .any(|update| update.generation_boundary.is_none())
    );

    let reset: Box<dyn Param + Send + Sync> = Box::new(ParamOutgoingResetRequest {
        reconfig_request_sequence_number: 9,
        reconfig_response_sequence_number: u32::MAX,
        sender_last_tsn: 5,
        stream_identifiers: vec![1],
    });
    a.handle_reconfig_param(&reset, &mut vec![])?;
    assert!(
        !a.deferred_forward_tsns.contains_key(&1),
        "resetting a drained tail must discard its retained ambiguous suffix"
    );
    Ok(())
}

#[test]
fn test_i_forward_tsn_skip_is_scoped_to_queued_reset_generation() -> Result<()> {
    let mut a = association_with_queued_second_reset()?;
    a.handle_i_forward_tsn(&ChunkIForwardTsn {
        new_cumulative_tsn: 3,
        streams: vec![ChunkIForwardTsnStream {
            identifier: 1,
            unordered: false,
            mid: 0,
        }],
    })?;
    assert_generation_c_ssn_zero_is_readable(a)
}

fn assert_later_reset_claims_tail_forward_tsn(mut a: Association) -> Result<()> {
    let second_reset: Box<dyn Param + Send + Sync> = Box::new(ParamOutgoingResetRequest {
        reconfig_request_sequence_number: 8,
        reconfig_response_sequence_number: u32::MAX,
        sender_last_tsn: 2,
        stream_identifiers: vec![1],
    });
    a.handle_reconfig_param(&second_reset, &mut vec![])?;

    let old = a.stream(1)?.read()?.unwrap();
    let mut payload = [0; 3];
    assert_eq!(old.read(&mut payload)?, payload.len());
    assert_eq!(&payload, b"old");

    a.handle_data(&ChunkPayloadData {
        tsn: 3,
        stream_identifier: 1,
        stream_sequence_number: 0,
        beginning_fragment: true,
        ending_fragment: true,
        user_data: Bytes::from_static(b"gen-c"),
        ..Default::default()
    })?;
    let generation_c = a.stream(1)?.read()?.unwrap();
    let mut payload = [0; 5];
    assert_eq!(generation_c.read(&mut payload)?, payload.len());
    assert_eq!(&payload, b"gen-c");
    Ok(())
}

#[test]
fn test_later_reset_claims_tail_forward_tsn_generation() -> Result<()> {
    let mut a = association_with_retiring_boundary_data()?;
    a.handle_forward_tsn(&ChunkForwardTsn {
        new_cumulative_tsn: 2,
        streams: vec![ChunkForwardTsnStream {
            identifier: 1,
            sequence: 0,
        }],
    })?;
    assert_later_reset_claims_tail_forward_tsn(a)
}

#[test]
fn test_later_reset_claims_tail_i_forward_tsn_generation() -> Result<()> {
    let mut a = association_with_retiring_boundary_data()?;
    a.handle_i_forward_tsn(&ChunkIForwardTsn {
        new_cumulative_tsn: 2,
        streams: vec![ChunkIForwardTsnStream {
            identifier: 1,
            unordered: false,
            mid: 0,
        }],
    })?;
    assert_later_reset_claims_tail_forward_tsn(a)
}

fn association_with_complete_deferred_successor() -> Result<Association> {
    let mut a = association_with_retiring_boundary_data()?;
    a.handle_data(&ChunkPayloadData {
        tsn: 2,
        stream_identifier: 1,
        stream_sequence_number: 0,
        beginning_fragment: true,
        ending_fragment: true,
        user_data: Bytes::from_static(b"received"),
        ..Default::default()
    })?;
    Ok(a)
}

fn assert_received_message_survives_forward_tsn(mut a: Association) -> Result<()> {
    let old = a.stream(1)?.read()?.unwrap();
    let mut payload = [0; 3];
    assert_eq!(old.read(&mut payload)?, payload.len());
    assert_eq!(&payload, b"old");

    let received = a.stream(1)?.read()?.unwrap();
    let mut payload = [0; 8];
    assert_eq!(received.read(&mut payload)?, payload.len());
    assert_eq!(&payload, b"received");
    Ok(())
}

#[test]
fn test_forward_tsn_preserves_complete_deferred_message() -> Result<()> {
    let mut a = association_with_complete_deferred_successor()?;
    a.handle_forward_tsn(&ChunkForwardTsn {
        // SSN 0 was received completely; only the following SSN 1 was
        // abandoned. RFC 3758 requires the stranded complete message to be
        // made available when the skip advances the ordered stream.
        new_cumulative_tsn: 3,
        streams: vec![ChunkForwardTsnStream {
            identifier: 1,
            sequence: 1,
        }],
    })?;
    assert_received_message_survives_forward_tsn(a)
}

#[test]
fn test_forward_tsn_preserves_out_of_order_complete_deferred_message() -> Result<()> {
    let mut a = association_with_retiring_boundary_data()?;

    // Generation B's complete SSN 0 arrives above a TSN gap, so it remains in
    // both the association payload queue and the reset-generation holding map.
    a.handle_data(&ChunkPayloadData {
        tsn: 3,
        stream_identifier: 1,
        stream_sequence_number: 0,
        beginning_fragment: true,
        ending_fragment: true,
        user_data: Bytes::from_static(b"received"),
        ..Default::default()
    })?;

    // The sender may not have received the gap SACK before abandoning TSNs 2
    // and 3. Advancing the cumulative TSN must not erase a complete message
    // that this receiver already holds for the successor generation.
    a.handle_forward_tsn(&ChunkForwardTsn {
        new_cumulative_tsn: 3,
        streams: vec![ChunkForwardTsnStream {
            identifier: 1,
            sequence: 0,
        }],
    })?;

    assert_received_message_survives_forward_tsn(a)
}

#[test]
fn test_i_forward_tsn_preserves_complete_deferred_message() -> Result<()> {
    let mut a = association_with_complete_deferred_successor()?;
    a.handle_i_forward_tsn(&ChunkIForwardTsn {
        new_cumulative_tsn: 3,
        streams: vec![ChunkIForwardTsnStream {
            identifier: 1,
            unordered: false,
            mid: 1,
        }],
    })?;
    assert_received_message_survives_forward_tsn(a)
}

fn association_with_deferred_unordered_fragment() -> Result<Association> {
    let mut a = association_with_retiring_boundary_data()?;
    a.streams.get_mut(&1).unwrap().unordered = true;
    a.handle_data(&ChunkPayloadData {
        tsn: 2,
        stream_identifier: 1,
        unordered: true,
        beginning_fragment: true,
        ending_fragment: false,
        user_data: Bytes::from_static(b"orphan"),
        ..Default::default()
    })?;
    Ok(a)
}

fn assert_abandoned_unordered_fragment_is_discarded(mut a: Association) -> Result<()> {
    let old = a.stream(1)?.read()?.unwrap();
    let mut payload = [0; 3];
    assert_eq!(old.read(&mut payload)?, payload.len());
    assert_eq!(&payload, b"old");

    assert_eq!(
        a.streams
            .get(&1)
            .map(StreamState::get_num_bytes_in_reassembly_queue)
            .unwrap_or_default(),
        0,
        "an abandoned partial unordered message must not survive replay"
    );
    assert_eq!(a.get_my_receiver_window_credit(), a.max_receive_buffer_size);
    Ok(())
}

#[test]
fn test_forward_tsn_discards_deferred_unordered_fragment() -> Result<()> {
    let mut a = association_with_deferred_unordered_fragment()?;
    a.handle_forward_tsn(&ChunkForwardTsn {
        new_cumulative_tsn: 3,
        streams: vec![],
    })?;
    assert_abandoned_unordered_fragment_is_discarded(a)
}

#[test]
fn test_i_forward_tsn_discards_deferred_unordered_fragment() -> Result<()> {
    let mut a = association_with_deferred_unordered_fragment()?;
    a.handle_i_forward_tsn(&ChunkIForwardTsn {
        new_cumulative_tsn: 3,
        streams: vec![ChunkIForwardTsnStream {
            identifier: 1,
            unordered: true,
            mid: 0,
        }],
    })?;
    assert_abandoned_unordered_fragment_is_discarded(a)
}

#[test]
fn test_forward_tsn_releases_abandoned_deferred_fragment_credit_immediately() -> Result<()> {
    let mut a = association_with_deferred_unordered_fragment()?;
    let old_generation_bytes = a
        .streams
        .get(&1)
        .unwrap()
        .get_num_bytes_in_reassembly_queue() as u32;

    a.handle_forward_tsn(&ChunkForwardTsn {
        new_cumulative_tsn: 3,
        streams: vec![],
    })?;

    assert_eq!(
        a.get_my_receiver_window_credit(),
        a.max_receive_buffer_size - old_generation_bytes,
        "abandoned successor fragments must not hold receive-window credit until the old generation drains"
    );
    Ok(())
}

#[test]
fn test_deferred_unordered_forward_tsn_updates_are_coalesced() -> Result<()> {
    let mut a = association_with_retiring_boundary_data()?;
    a.streams.get_mut(&1).unwrap().unordered = true;

    for new_cumulative_tsn in 2..=9 {
        a.handle_forward_tsn(&ChunkForwardTsn {
            new_cumulative_tsn,
            streams: vec![],
        })?;
    }

    let updates = a.deferred_forward_tsns.get(&1).unwrap();
    assert_eq!(
        updates.len(),
        1,
        "newer unordered cumulative TSNs supersede older updates for the same generation"
    );
    assert!(matches!(
        updates.front(),
        Some(DeferredForwardTsn {
            kind: DeferredForwardTsnKind::Unordered {
                new_cumulative_tsn: 9
            },
            ..
        })
    ));
    Ok(())
}

#[test]
fn test_deferred_tail_ordered_forward_tsn_duplicates_are_coalesced() -> Result<()> {
    let mut a = association_with_retiring_boundary_data()?;
    for new_cumulative_tsn in 2..=9 {
        a.handle_forward_tsn(&ChunkForwardTsn {
            new_cumulative_tsn,
            streams: vec![ChunkForwardTsnStream {
                identifier: 1,
                sequence: 0,
            }],
        })?;
    }

    let ordered: Vec<_> = a.deferred_forward_tsns[&1]
        .iter()
        .filter(|update| {
            update.generation_boundary.is_none()
                && matches!(update.kind, DeferredForwardTsnKind::Ordered { .. })
        })
        .collect();
    assert_eq!(
        ordered.len(),
        1,
        "lost-SACK repeats for an ambiguous tail SSN should coalesce"
    );
    Ok(())
}

#[test]
fn test_deferred_bounded_ordered_forward_tsn_updates_are_coalesced() -> Result<()> {
    let mut a = association_with_retiring_boundary_data()?;
    let reset: Box<dyn Param + Send + Sync> = Box::new(ParamOutgoingResetRequest {
        reconfig_request_sequence_number: 8,
        reconfig_response_sequence_number: u32::MAX,
        sender_last_tsn: 10,
        stream_identifiers: vec![1],
    });
    a.handle_reconfig_param(&reset, &mut vec![])?;

    for new_cumulative_tsn in 2..=9 {
        a.handle_forward_tsn(&ChunkForwardTsn {
            new_cumulative_tsn,
            streams: vec![ChunkForwardTsnStream {
                identifier: 1,
                sequence: (new_cumulative_tsn - 2) as u16,
            }],
        })?;
    }

    let ordered: Vec<_> = a.deferred_forward_tsns[&1]
        .iter()
        .filter(|update| {
            update.generation_boundary == Some(10)
                && matches!(update.kind, DeferredForwardTsnKind::Ordered { .. })
        })
        .collect();
    assert_eq!(
        ordered.len(),
        1,
        "a newer ordered skip supersedes older skips for the same reset boundary"
    );
    assert!(matches!(
        ordered[0].kind,
        DeferredForwardTsnKind::Ordered { last_ssn: 7, .. }
    ));
    Ok(())
}

#[test]
fn test_unread_retiring_stream_does_not_block_unrelated_events() -> Result<()> {
    let mut a = association_with_retiring_boundary_data()?;
    assert!(
        a.create_stream(2, false, PayloadProtocolIdentifier::Binary)
            .is_some()
    );

    assert!(
        core::iter::from_fn(|| a.poll())
            .any(|event| matches!(event, Event::Stream(StreamEvent::Readable { id: 1 })))
    );
    assert!(a.poll().is_none(), "stream 1 Finished should still wait");

    a.handle_data(&ChunkPayloadData {
        tsn: 2,
        stream_identifier: 2,
        stream_sequence_number: 0,
        beginning_fragment: true,
        ending_fragment: true,
        user_data: Bytes::from_static(b"other"),
        ..Default::default()
    })?;

    assert!(matches!(a.poll(), Some(Event::DatagramReceived)));
    assert!(matches!(
        a.poll(),
        Some(Event::Stream(StreamEvent::Readable { id: 2 }))
    ));
    Ok(())
}

#[test]
fn test_stop_discards_unread_retiring_data_and_unblocks_finished() -> Result<()> {
    let mut a = association_with_retiring_boundary_data()?;
    assert!(
        core::iter::from_fn(|| a.poll())
            .any(|event| matches!(event, Event::Stream(StreamEvent::Readable { id: 1 })))
    );
    assert!(a.poll().is_none(), "Finished should wait for old DATA");

    a.stream(1)?.stop()?;
    assert!(matches!(
        a.poll(),
        Some(Event::Stream(StreamEvent::Finished { id: 1 }))
    ));
    Ok(())
}

#[test]
fn test_stop_during_retirement_does_not_queue_duplicate_reset() -> Result<()> {
    let mut a = association_with_retiring_boundary_data()?;
    assert_eq!(
        a.reconfigs.len(),
        1,
        "peer reset should queue one reciprocal"
    );
    assert!(a.pending_reset_streams.is_empty());

    a.stream(1)?.stop()?;

    assert!(
        a.pending_reset_streams.is_empty(),
        "the reciprocal already resets this outgoing stream"
    );
    assert_eq!(a.reconfigs.len(), 1);
    Ok(())
}

fn assert_shutdown_during_retirement_allows_reuse(close: bool) -> Result<()> {
    let mut a = association_with_retiring_boundary_data()?;
    if close {
        a.stream(1)?.close()?;
    } else {
        a.stream(1)?.stop()?;
    }

    let rsn = *a.reconfigs.keys().next().unwrap();
    a.active_reconfig = Some(rsn);
    let response: Box<dyn Param + Send + Sync> = Box::new(ParamReconfigResponse {
        reconfig_response_sequence_number: rsn,
        result: ReconfigResult::SuccessPerformed,
    });
    a.handle_reconfig_param(&response, &mut vec![])?;

    assert!(
        !a.streams.contains_key(&1),
        "a completed reciprocal reset must remove the locally shut down stream"
    );
    assert!(
        a.open_stream(1, PayloadProtocolIdentifier::Binary).is_ok(),
        "ResetComplete must make the retired stream id reusable"
    );
    Ok(())
}

#[test]
fn test_stop_during_retirement_allows_reuse_after_reset_complete() -> Result<()> {
    assert_shutdown_during_retirement_allows_reuse(false)
}

#[test]
fn test_close_during_retirement_allows_reuse_after_reset_complete() -> Result<()> {
    assert_shutdown_during_retirement_allows_reuse(true)
}

fn assert_reset_complete_before_shutdown_allows_reuse(close: bool) -> Result<()> {
    let mut a = association_with_retiring_boundary_data()?;
    let rsn = *a.reconfigs.keys().next().unwrap();
    a.active_reconfig = Some(rsn);
    let response: Box<dyn Param + Send + Sync> = Box::new(ParamReconfigResponse {
        reconfig_response_sequence_number: rsn,
        result: ReconfigResult::SuccessPerformed,
    });
    a.handle_reconfig_param(&response, &mut vec![])?;

    if close {
        a.stream(1)?.close()?;
    } else {
        a.stream(1)?.stop()?;
    }

    assert!(
        a.pending_reset_streams.is_empty() && a.reconfigs.is_empty(),
        "shutting down a completed retiring stream must not queue a duplicate reset"
    );
    assert!(
        !a.streams.contains_key(&1),
        "discarding the retired generation after ResetComplete must remove the stream"
    );
    assert!(
        a.open_stream(1, PayloadProtocolIdentifier::Binary).is_ok(),
        "the completed stream id should be immediately reusable"
    );
    Ok(())
}

#[test]
fn test_reset_complete_before_stop_allows_reuse() -> Result<()> {
    assert_reset_complete_before_shutdown_allows_reuse(false)
}

#[test]
fn test_reset_complete_before_close_allows_reuse() -> Result<()> {
    assert_reset_complete_before_shutdown_allows_reuse(true)
}

#[test]
fn test_stop_does_not_reopen_read_half_on_deferred_successor() -> Result<()> {
    let mut a = association_with_retiring_boundary_data()?;
    a.handle_data(&ChunkPayloadData {
        tsn: 2,
        stream_identifier: 1,
        stream_sequence_number: 0,
        beginning_fragment: true,
        ending_fragment: true,
        user_data: Bytes::from_static(b"successor"),
        ..Default::default()
    })?;

    let mut stream = a.stream(1)?;
    stream.stop()?;
    assert_eq!(stream.read().unwrap_err(), Error::ErrStreamClosed);
    Ok(())
}

#[test]
fn test_stop_discards_deferred_successor_bytes_and_notifications() -> Result<()> {
    let mut a = association_with_retiring_boundary_data()?;
    while a.poll().is_some() {}
    a.handle_data(&ChunkPayloadData {
        tsn: 2,
        stream_identifier: 1,
        stream_sequence_number: 0,
        beginning_fragment: true,
        ending_fragment: true,
        user_data: Bytes::from_static(b"successor"),
        ..Default::default()
    })?;

    a.stream(1)?.stop()?;

    assert_eq!(
        a.streams
            .get(&1)
            .unwrap()
            .get_num_bytes_in_reassembly_queue(),
        0,
        "stop must not strand bytes in a write-only successor"
    );
    assert_eq!(a.get_my_receiver_window_credit(), a.max_receive_buffer_size);
    assert!(
        !core::iter::from_fn(|| a.poll()).any(|event| matches!(
            event,
            Event::Stream(StreamEvent::Opened { id } | StreamEvent::Readable { id }) if id == 1
        )),
        "stop must not advertise an unreadable successor"
    );
    Ok(())
}

#[test]
fn test_finish_remains_closed_across_deferred_successor() -> Result<()> {
    let mut a = association_with_retiring_boundary_data()?;
    a.handle_data(&ChunkPayloadData {
        tsn: 2,
        stream_identifier: 1,
        stream_sequence_number: 0,
        beginning_fragment: true,
        ending_fragment: true,
        user_data: Bytes::from_static(b"successor"),
        ..Default::default()
    })?;
    a.stream(1)?.finish()?;

    let old = a.stream(1)?.read()?.unwrap();
    let mut payload = [0; 3];
    assert_eq!(old.read(&mut payload)?, payload.len());
    assert_eq!(&payload, b"old");
    assert_eq!(a.streams.get(&1).unwrap().state, RecvSendState::Readable);

    let successor = a.stream(1)?.read()?.unwrap();
    let mut payload = [0; 9];
    assert_eq!(successor.read(&mut payload)?, payload.len());
    assert_eq!(&payload, b"successor");
    Ok(())
}

#[test]
fn test_close_remains_closed_across_deferred_successor() -> Result<()> {
    let mut a = association_with_retiring_boundary_data()?;
    a.handle_data(&ChunkPayloadData {
        tsn: 2,
        stream_identifier: 1,
        stream_sequence_number: 0,
        beginning_fragment: true,
        ending_fragment: true,
        user_data: Bytes::from_static(b"successor"),
        ..Default::default()
    })?;

    a.stream(1)?.close()?;

    let successor = a.streams.get(&1).unwrap();
    assert_eq!(successor.state, RecvSendState::Closed);
    assert_eq!(successor.get_num_bytes_in_reassembly_queue(), 0);
    assert_eq!(a.get_my_receiver_window_credit(), a.max_receive_buffer_size);
    Ok(())
}

#[test]
fn test_duplicate_stream_ids_retire_only_one_generation() -> Result<()> {
    let stream_id = 1;
    let mut a = Association {
        state: AssociationState::Established,
        peer_last_tsn: 0,
        my_next_tsn: 1,
        max_receive_buffer_size: 1024,
        max_receive_message_size: 1024,
        max_payload_size: 1200,
        ..Default::default()
    };
    assert!(
        a.create_stream(stream_id, false, PayloadProtocolIdentifier::Binary)
            .is_some()
    );
    a.handle_data(&ChunkPayloadData {
        tsn: 1,
        stream_identifier: stream_id,
        stream_sequence_number: 0,
        beginning_fragment: true,
        ending_fragment: true,
        user_data: Bytes::from_static(b"old"),
        ..Default::default()
    })?;

    let reset: Box<dyn Param + Send + Sync> = Box::new(ParamOutgoingResetRequest {
        reconfig_request_sequence_number: 7,
        reconfig_response_sequence_number: u32::MAX,
        sender_last_tsn: 1,
        stream_identifiers: vec![stream_id, stream_id],
    });
    a.handle_reconfig_param(&reset, &mut vec![])?;

    assert_eq!(a.retiring_streams.get(&stream_id).unwrap().len(), 1);
    assert_eq!(a.pending_reset_completions.0.get(&stream_id), Some(&1));
    assert_eq!(
        a.reconfigs
            .values()
            .flat_map(Association::reconfig_stream_ids)
            .collect::<Vec<_>>(),
        vec![stream_id]
    );
    Ok(())
}

#[test]
fn test_reconfig_rejects_two_outgoing_resets_without_mutation() -> Result<()> {
    let mut a = association_with_retiring_boundary_data()?;
    let boundaries_before = a.retiring_streams.get(&1).unwrap().len();
    let completions_before = a.pending_reset_completions.0.get(&1).copied();
    let reconfigs_before = a.reconfigs.len();

    let reconfig = ChunkReconfig {
        param_a: Some(Box::new(ParamOutgoingResetRequest {
            reconfig_request_sequence_number: 8,
            reconfig_response_sequence_number: u32::MAX,
            sender_last_tsn: a.peer_last_tsn,
            stream_identifiers: vec![1],
        })),
        param_b: Some(Box::new(ParamOutgoingResetRequest {
            reconfig_request_sequence_number: 9,
            reconfig_response_sequence_number: u32::MAX,
            sender_last_tsn: a.peer_last_tsn,
            stream_identifiers: vec![1],
        })),
    };
    let packet = a.create_packet(vec![Box::new(reconfig)]);
    let result = a.handle_chunk(&packet, &packet.chunks[0], Instant::now());

    assert!(result.is_err(), "two Outgoing Reset parameters are invalid");
    assert_eq!(a.retiring_streams.get(&1).unwrap().len(), boundaries_before);
    assert_eq!(
        a.pending_reset_completions.0.get(&1).copied(),
        completions_before
    );
    assert_eq!(a.reconfigs.len(), reconfigs_before);
    Ok(())
}

#[test]
fn test_stop_before_reset_boundary_does_not_leave_finished_blocked() -> Result<()> {
    let stream_id = 1;
    let mut a = Association {
        state: AssociationState::Established,
        peer_last_tsn: 0,
        my_next_tsn: 1,
        max_receive_buffer_size: 1024,
        max_receive_message_size: 1024,
        max_payload_size: 1200,
        ..Default::default()
    };
    for id in [stream_id, 2] {
        assert!(
            a.create_stream(id, false, PayloadProtocolIdentifier::Binary)
                .is_some()
        );
    }

    // TSN 2 is readable but cannot advance the cumulative point past missing
    // TSN 1, leaving time for the application to stop the read half.
    a.handle_data(&ChunkPayloadData {
        tsn: 2,
        stream_identifier: stream_id,
        stream_sequence_number: 0,
        beginning_fragment: true,
        ending_fragment: true,
        user_data: Bytes::from_static(b"discard"),
        ..Default::default()
    })?;
    let reset: Box<dyn Param + Send + Sync> = Box::new(ParamOutgoingResetRequest {
        reconfig_request_sequence_number: 7,
        reconfig_response_sequence_number: u32::MAX,
        sender_last_tsn: 2,
        stream_identifiers: vec![stream_id],
    });
    a.handle_reconfig_param(&reset, &mut vec![])?;
    a.stream(stream_id)?.stop()?;

    a.handle_data(&ChunkPayloadData {
        tsn: 1,
        stream_identifier: 2,
        stream_sequence_number: 0,
        beginning_fragment: true,
        ending_fragment: true,
        user_data: Bytes::from_static(b"gap"),
        ..Default::default()
    })?;

    assert!(a.stream(stream_id).is_err());
    assert!(
        core::iter::from_fn(|| a.poll()).any(|event| matches!(
            event,
            Event::Stream(StreamEvent::Finished { id }) if id == stream_id
        )),
        "a stopped read half cannot be left waiting for an impossible drain"
    );
    Ok(())
}

#[test]
fn test_oversized_deferred_successor_is_rejected_before_old_read() -> Result<()> {
    let mut a = association_with_retiring_boundary_data()?;
    a.max_receive_message_size = 3;

    let error = a
        .handle_data(&ChunkPayloadData {
            tsn: 2,
            stream_identifier: 1,
            stream_sequence_number: 0,
            beginning_fragment: true,
            ending_fragment: true,
            user_data: Bytes::from_static(b"too-big"),
            ..Default::default()
        })
        .unwrap_err();
    assert_eq!(error, Error::ErrInboundPacketTooLarge);

    let old = a.stream(1)?.read()?.unwrap();
    let mut payload = [0; 3];
    assert_eq!(old.read(&mut payload)?, payload.len());
    assert_eq!(&payload, b"old");
    Ok(())
}

#[test]
fn test_future_peer_reconfig_sequence_is_rejected() -> Result<()> {
    let stream_id = 1;
    let mut a = Association {
        peer_last_tsn: 0,
        peer_last_reconfig_rsn: 40,
        peer_reconfig_rsn_initialized: true,
        my_next_tsn: 1,
        ..Default::default()
    };
    assert!(
        a.create_stream(stream_id, false, PayloadProtocolIdentifier::Binary)
            .is_some()
    );

    let future: Box<dyn Param + Send + Sync> = Box::new(ParamOutgoingResetRequest {
        reconfig_request_sequence_number: 42,
        reconfig_response_sequence_number: u32::MAX,
        sender_last_tsn: a.peer_last_tsn,
        stream_identifiers: vec![stream_id],
    });
    let mut reply = vec![];
    a.handle_reconfig_param(&future, &mut reply)?;

    assert_eq!(
        reconfig_response_result(&reply, 42),
        Some(ReconfigResult::ErrorBadSequenceNumber)
    );
    assert_eq!(a.peer_last_reconfig_rsn, 40);
    assert!(a.max_completed_reconfig_rsn.is_none());
    assert!(a.streams.contains_key(&stream_id));

    let expected: Box<dyn Param + Send + Sync> = Box::new(ParamOutgoingResetRequest {
        reconfig_request_sequence_number: 41,
        reconfig_response_sequence_number: u32::MAX,
        sender_last_tsn: a.peer_last_tsn,
        stream_identifiers: vec![stream_id],
    });
    a.handle_reconfig_param(&expected, &mut vec![])?;
    assert!(!a.streams.contains_key(&stream_id));
    assert_eq!(a.max_completed_reconfig_rsn, Some(41));
    Ok(())
}

#[test]
fn test_peer_reconfig_sequence_accepts_wrap_to_zero() -> Result<()> {
    let stream_id = 1;
    let mut a = Association {
        peer_last_tsn: 0,
        peer_last_reconfig_rsn: u32::MAX,
        peer_reconfig_rsn_initialized: true,
        my_next_tsn: 1,
        ..Default::default()
    };
    assert!(
        a.create_stream(stream_id, false, PayloadProtocolIdentifier::Binary)
            .is_some()
    );

    let request: Box<dyn Param + Send + Sync> = Box::new(ParamOutgoingResetRequest {
        reconfig_request_sequence_number: 0,
        reconfig_response_sequence_number: u32::MAX,
        sender_last_tsn: a.peer_last_tsn,
        stream_identifiers: vec![stream_id],
    });
    let mut reply = vec![];
    a.handle_reconfig_param(&request, &mut reply)?;

    assert_ne!(
        reconfig_response_result(&reply, 0),
        Some(ReconfigResult::ErrorBadSequenceNumber)
    );
    assert_eq!(a.peer_last_reconfig_rsn, 0);
    assert_eq!(a.max_completed_reconfig_rsn, Some(0));
    assert!(!a.streams.contains_key(&stream_id));
    Ok(())
}

#[test]
fn test_empty_reset_stream_list_resets_all_streams() -> Result<()> {
    let mut a = Association {
        my_next_tsn: 1,
        ..Default::default()
    };
    for stream_id in [1, 2] {
        assert!(
            a.create_stream(stream_id, false, PayloadProtocolIdentifier::Binary)
                .is_some()
        );
    }

    let request: Box<dyn Param + Send + Sync> = Box::new(ParamOutgoingResetRequest {
        reconfig_request_sequence_number: 7,
        reconfig_response_sequence_number: u32::MAX,
        sender_last_tsn: a.peer_last_tsn,
        stream_identifiers: vec![],
    });
    let mut reply = vec![];
    a.handle_reconfig_param(&request, &mut reply)?;

    assert!(
        a.streams.is_empty(),
        "an omitted stream list means all streams"
    );
    let reciprocal_ids = a
        .reconfigs
        .values()
        .next()
        .map(Association::reconfig_stream_ids)
        .unwrap_or_default();
    assert!(
        reciprocal_ids.is_empty(),
        "reset-all must remain compact on the wire"
    );
    let mut completion_ids = a
        .reconfig_reset_streams
        .values()
        .next()
        .cloned()
        .unwrap_or_default();
    completion_ids.sort_unstable();
    assert_eq!(completion_ids, vec![1, 2]);
    Ok(())
}

#[test]
fn test_reset_all_reciprocal_fits_reconfig_chunk_length() -> Result<()> {
    let mut a = Association {
        my_next_tsn: 1,
        ..Default::default()
    };
    for stream_id in 0..32_758u32 {
        assert!(
            a.create_stream(
                stream_id as StreamId,
                false,
                PayloadProtocolIdentifier::Binary,
            )
            .is_some()
        );
    }

    let request: Box<dyn Param + Send + Sync> = Box::new(ParamOutgoingResetRequest {
        reconfig_request_sequence_number: 7,
        reconfig_response_sequence_number: u32::MAX,
        sender_last_tsn: a.peer_last_tsn,
        stream_identifiers: vec![],
    });
    let mut reply = vec![];
    a.handle_reconfig_param(&request, &mut reply)?;

    for packet in reply {
        packet.marshal()?;
    }
    Ok(())
}

#[test]
fn test_blocked_reconfig_does_not_allow_later_rsn_to_overtake() {
    let mut a = Association {
        state: AssociationState::Established,
        my_next_tsn: 1,
        cwnd: 0,
        rwnd: 0,
        mtu: 1400,
        ..Default::default()
    };
    queue_inflight(
        &mut a,
        ChunkPayloadData {
            tsn: 0,
            stream_identifier: 2,
            user_data: Bytes::from_static(b"inflight"),
            ..Default::default()
        },
    );
    queue_pending(
        &mut a,
        ChunkPayloadData {
            stream_identifier: 1,
            beginning_fragment: true,
            ending_fragment: true,
            user_data: Bytes::from_static(b"pending"),
            ..Default::default()
        },
    );
    insert_queued_reset(&mut a, 7, 1);
    insert_queued_reset(&mut a, 8, 2);

    let _ = a.gather_outbound(Instant::now());
    assert!(
        a.active_reconfig.is_none(),
        "RSN 8 must not overtake RSN 7 while RSN 7 waits for its DATA boundary"
    );
    assert_eq!(a.control_queue.len(), 2);

    a.cwnd = 1400;
    a.rwnd = 1400;
    let _ = a.gather_outbound(Instant::now());
    assert_eq!(a.active_reconfig, Some(7));
    assert_eq!(a.control_queue.len(), 1);
}

#[test]
fn test_retransmission_does_not_send_buffered_reconfigs() {
    let mut a = Association {
        state: AssociationState::Established,
        ..Default::default()
    };

    insert_active_reset(&mut a, 7, 1);
    insert_queued_reset(&mut a, 8, 2);
    a.timers
        .start(Timer::Reconfig, Instant::now(), a.rto_mgr.get_rto());
    a.will_retransmit_reconfig = true;

    let (packets, _) = a.gather_outbound(Instant::now());
    assert_eq!(
        packets.len(),
        1,
        "retransmission must include only the request associated with the running timer"
    );
}

fn association_with_inflight_data(cumulative_tsn_ack_point: u32, now: Instant) -> Association {
    let mut a = Association {
        state: AssociationState::Established,
        cumulative_tsn_ack_point,
        advanced_peer_tsn_ack_point: cumulative_tsn_ack_point,
        my_next_tsn: cumulative_tsn_ack_point.wrapping_add(4),
        cwnd: 1400,
        rwnd: 1400,
        mtu: 1400,
        ..Default::default()
    };

    for stream_sequence_number in 0..3 {
        queue_inflight(
            &mut a,
            ChunkPayloadData {
                tsn: cumulative_tsn_ack_point.wrapping_add(u32::from(stream_sequence_number) + 1),
                stream_identifier: 1,
                stream_sequence_number,
                beginning_fragment: true,
                ending_fragment: true,
                payload_type: PayloadProtocolIdentifier::Binary,
                user_data: Bytes::from_static(b"payload"),
                nsent: 1,
                since: Some(now),
                ..Default::default()
            },
        );
        let tsn = cumulative_tsn_ack_point.wrapping_add(u32::from(stream_sequence_number) + 1);
        a.outbound_queue.get_mut(tsn).unwrap().since = Some(now);
    }

    a
}

fn assert_retransmitted_packet(packet: &Packet, expected_tsns: &[u32]) {
    assert_eq!(packet.chunks.len(), expected_tsns.len());
    for (chunk, expected_tsn) in packet.chunks.iter().zip(expected_tsns) {
        let data = chunk.as_any().downcast_ref::<ChunkPayloadData>().unwrap();
        assert_eq!(data.tsn, *expected_tsn);
        assert_eq!(data.user_data, Bytes::from_static(b"payload"));
    }
}

#[test]
fn test_data_retransmission_wraps_tsn() -> Result<()> {
    let now = Instant::now();
    for cumulative_tsn in [10, u32::MAX - 1, u32::MAX] {
        let mut a = association_with_inflight_data(cumulative_tsn, now);
        let acked_tsn = cumulative_tsn.wrapping_add(2);
        a.outbound_queue.mark_as_acked(acked_tsn);
        a.on_retransmission_timeout(Timer::T3RTX, 1);

        let packets = a.get_data_packets_to_retransmit(now);
        assert_eq!(packets.len(), 1);
        let packet = Packet::unmarshal(&packets[0].marshal()?)?;
        let retransmitted_tsns = [
            cumulative_tsn.wrapping_add(1),
            cumulative_tsn.wrapping_add(3),
        ];
        assert_retransmitted_packet(&packet, &retransmitted_tsns);
        for tsn in retransmitted_tsns {
            let chunk = a.outbound_queue.get(tsn).unwrap();
            assert!(!chunk.retransmit);
            assert_eq!(chunk.nsent, 2);
        }
        assert_eq!(a.outbound_queue.get(acked_tsn).unwrap().nsent, 1);
        assert!(a.get_data_packets_to_retransmit(now).is_empty());
    }
    Ok(())
}

#[test]
fn test_poll_transmit_at_tsn_wraparound() {
    let mut a = Association {
        state: AssociationState::Established,
        cumulative_tsn_ack_point: u32::MAX,
        advanced_peer_tsn_ack_point: u32::MAX,
        ..Default::default()
    };

    assert!(a.poll_transmit(Instant::now()).is_none());
}

#[test]
fn test_zero_window_probe_wraps_tsn() -> Result<()> {
    let now = Instant::now();
    for cumulative_tsn in [u32::MAX - 1, u32::MAX] {
        let mut a = association_with_inflight_data(cumulative_tsn, now);
        a.rwnd = 0;
        a.on_retransmission_timeout(Timer::T3RTX, 1);

        let packets = a.get_data_packets_to_retransmit(now);
        assert_eq!(packets.len(), 1);
        assert_retransmitted_packet(&packets[0], &[cumulative_tsn.wrapping_add(1)]);
        for offset in [2, 3] {
            let chunk = a
                .outbound_queue
                .get(cumulative_tsn.wrapping_add(offset))
                .unwrap();
            assert!(chunk.retransmit);
            assert_eq!(chunk.nsent, 1);
        }
        assert!(a.get_data_packets_to_retransmit(now).is_empty());
    }
    Ok(())
}

#[test]
fn test_fast_retransmission_wraps_tsn() -> Result<()> {
    let now = Instant::now();
    for cumulative_tsn in [10, u32::MAX - 1, u32::MAX] {
        let mut a = association_with_inflight_data(cumulative_tsn, now);
        let acked_tsn = cumulative_tsn.wrapping_add(2);
        a.outbound_queue.mark_as_acked(acked_tsn);
        let retransmitted_tsns = [
            cumulative_tsn.wrapping_add(1),
            cumulative_tsn.wrapping_add(3),
        ];
        for tsn in retransmitted_tsns {
            a.outbound_queue.get_mut(tsn).unwrap().miss_indicator = 3;
        }
        a.will_retransmit_fast = true;

        let packets = a.gather_outbound_fast_retransmission_packets(vec![], now);
        assert_eq!(packets.len(), 1);
        assert_retransmitted_packet(&Packet::unmarshal(&packets[0])?, &retransmitted_tsns);
        for tsn in retransmitted_tsns {
            assert_eq!(a.outbound_queue.get(tsn).unwrap().nsent, 2);
        }
        assert_eq!(a.outbound_queue.get(acked_tsn).unwrap().nsent, 1);
        assert!(!a.will_retransmit_fast);
        assert_eq!(a.stats.get_num_fast_retrans(), 2);
    }
    Ok(())
}

#[test]
fn test_cumulative_sack_wraps_tsn() -> Result<()> {
    let now = Instant::now();
    for cumulative_tsn in [10, u32::MAX - 1, u32::MAX] {
        let mut a = association_with_inflight_data(cumulative_tsn, now);
        a.timers.start(Timer::T3RTX, now, a.rto_mgr.get_rto());
        let ack_tsn = cumulative_tsn.wrapping_add(2);
        let (bytes_acked, htna) = a.process_selective_ack(
            &ChunkSelectiveAck {
                cumulative_tsn_ack: ack_tsn,
                ..Default::default()
            },
            now,
        )?;

        assert_eq!(bytes_acked, HashMap::from([(1, 14)]));
        assert_eq!(htna, ack_tsn);
        assert_eq!(a.outbound_queue.inflight_len(), 1);
        assert!(
            a.outbound_queue
                .get(cumulative_tsn.wrapping_add(3))
                .is_some()
        );
        assert!(a.timers.get(Timer::T3RTX).is_none());
    }
    Ok(())
}

#[test]
fn test_gap_sack_wraps_tsn() -> Result<()> {
    let now = Instant::now();
    for cumulative_tsn in [10, u32::MAX - 1, u32::MAX] {
        let mut sender = association_with_inflight_data(cumulative_tsn, now);
        let mut receiver = Association {
            peer_last_tsn: cumulative_tsn,
            ..Default::default()
        };
        for offset in [2, 3] {
            let data = sender
                .outbound_queue
                .get(cumulative_tsn.wrapping_add(offset))
                .unwrap()
                .clone();
            assert!(receiver.payload_queue.push(data, cumulative_tsn));
        }
        let sack = receiver.create_selective_ack_chunk();
        let (bytes_acked, htna) = sender.process_selective_ack(&sack, now)?;

        assert_eq!(bytes_acked, HashMap::from([(1, 14)]));
        assert_eq!(htna, cumulative_tsn.wrapping_add(3));
        assert_eq!(sender.outbound_queue.inflight_bytes(), 7);
        for offset in [1, 2, 3] {
            let data = sender
                .outbound_queue
                .get(cumulative_tsn.wrapping_add(offset))
                .unwrap();
            assert_eq!(data.acked, offset != 1);
        }
    }
    Ok(())
}

#[test]
fn test_fast_recovery_wraps_tsn() -> Result<()> {
    let now = Instant::now();
    for cumulative_tsn in [10, u32::MAX - 1, u32::MAX] {
        for in_fast_recovery in [false, true] {
            let mut a = association_with_inflight_data(cumulative_tsn, now);
            a.in_fast_recovery = in_fast_recovery;
            let htna = cumulative_tsn.wrapping_add(3);
            a.fast_recover_exit_point = htna;
            a.outbound_queue.mark_as_acked(htna);
            for offset in [1, 2] {
                a.outbound_queue
                    .get_mut(cumulative_tsn.wrapping_add(offset))
                    .unwrap()
                    .miss_indicator = 2;
            }

            a.process_fast_retransmission(cumulative_tsn, htna, true)?;

            assert!(a.in_fast_recovery);
            assert!(a.will_retransmit_fast);
            assert_eq!(a.fast_recover_exit_point, htna);
            for offset in [1, 2] {
                let chunk = a
                    .outbound_queue
                    .get(cumulative_tsn.wrapping_add(offset))
                    .unwrap();
                assert_eq!(chunk.miss_indicator, 3);
            }
            assert_eq!(a.outbound_queue.get(htna).unwrap().miss_indicator, 0);
        }
    }
    Ok(())
}

#[test]
fn test_forward_tsn_wraparound() -> Result<()> {
    let now = Instant::now();
    for cumulative_tsn in [10, u32::MAX - 1, u32::MAX] {
        for from_timeout in [false, true] {
            let mut a = association_with_inflight_data(cumulative_tsn, now);
            a.use_forward_tsn = true;
            for offset in [1, 2] {
                a.outbound_queue
                    .get_mut(cumulative_tsn.wrapping_add(offset))
                    .unwrap()
                    .abandoned = true;
            }

            if from_timeout {
                a.on_retransmission_timeout(Timer::T3RTX, 1);
            } else {
                a.handle_sack(
                    &ChunkSelectiveAck {
                        cumulative_tsn_ack: cumulative_tsn,
                        advertised_receiver_window_credit: 1400,
                        ..Default::default()
                    },
                    now,
                )?;
            }

            assert_eq!(
                a.advanced_peer_tsn_ack_point,
                cumulative_tsn.wrapping_add(2)
            );
            assert!(a.will_send_forward_tsn);
            let packets = a.gather_outbound_forward_tsn_packets(vec![]);
            assert_eq!(packets.len(), 1);
            let packet = Packet::unmarshal(&packets[0])?;
            assert_eq!(packet.chunks.len(), 1);
            let forward = packet.chunks[0]
                .as_any()
                .downcast_ref::<ChunkForwardTsn>()
                .unwrap();
            assert_eq!(forward.new_cumulative_tsn, cumulative_tsn.wrapping_add(2));
            assert_eq!(forward.streams.len(), 1);
            assert_eq!(forward.streams[0].identifier, 1);
            assert_eq!(forward.streams[0].sequence, 1);
            assert!(!a.will_send_forward_tsn);
        }
    }
    Ok(())
}

#[test]
fn test_older_completion_does_not_make_newer_generation_writable() -> Result<()> {
    let stream_id = 1;
    let mut a = Association::default();

    a.pending_reset_completions.insert(stream_id);
    a.pending_reset_completions.insert(stream_id);
    insert_active_reset(&mut a, 7, stream_id);
    insert_queued_reset(&mut a, 8, stream_id);
    a.timers
        .start(Timer::Reconfig, Instant::now(), a.rto_mgr.get_rto());

    assert!(a.get_or_create_stream(stream_id).is_some());
    assert!(!a.stream(stream_id)?.is_writable());

    let response: Box<dyn Param + Send + Sync> = Box::new(ParamReconfigResponse {
        reconfig_response_sequence_number: 7,
        result: ReconfigResult::SuccessPerformed,
    });
    a.handle_reconfig_param(&response, &mut vec![])?;

    assert!(a.reconfigs.contains_key(&8));
    assert!(
        !a.stream(stream_id)?.is_writable(),
        "an older success cannot release the newer generation's outgoing quarantine"
    );
    Ok(())
}

#[test]
fn test_stale_completion_does_not_override_newer_denial() -> Result<()> {
    let stream_id = 1;
    let mut a = Association::default();

    a.pending_reset_completions.insert(stream_id);
    insert_active_reset(&mut a, 7, stream_id);

    let success: Box<dyn Param + Send + Sync> = Box::new(ParamReconfigResponse {
        reconfig_response_sequence_number: 7,
        result: ReconfigResult::SuccessPerformed,
    });
    a.handle_reconfig_param(&success, &mut vec![])?;

    // Before the application polls generation 7's completion, the peer opens
    // and uses generation 8, then resets it. Its nonzero outgoing sequence
    // means a denied reciprocal reset must quarantine the id.
    assert!(a.get_or_create_stream(stream_id).is_some());
    a.streams.get_mut(&stream_id).unwrap().sequence_number = 1;
    a.unregister_stream(stream_id, true);
    insert_active_reset(&mut a, 8, stream_id);
    a.timers
        .start(Timer::Reconfig, Instant::now(), a.rto_mgr.get_rto());

    let denied: Box<dyn Param + Send + Sync> = Box::new(ParamReconfigResponse {
        reconfig_response_sequence_number: 8,
        result: ReconfigResult::Denied,
    });
    a.handle_reconfig_param(&denied, &mut vec![])?;

    // Polling the stale completion must not resurrect reuse permission after
    // generation 8 has already failed.
    assert!(matches!(
        a.poll(),
        Some(Event::Stream(StreamEvent::ResetComplete { id })) if id == stream_id
    ));

    assert!(matches!(
        a.open_stream(stream_id, PayloadProtocolIdentifier::Binary),
        Err(Error::ErrStreamResetPending)
    ));
    Ok(())
}

#[test]
fn test_create_forward_tsn_forward_one_abandoned() -> Result<()> {
    let mut a = Association {
        cumulative_tsn_ack_point: 9,
        advanced_peer_tsn_ack_point: 10,
        ..Default::default()
    };

    queue_inflight(
        &mut a,
        ChunkPayloadData {
            beginning_fragment: true,
            ending_fragment: true,
            tsn: 10,
            stream_identifier: 1,
            stream_sequence_number: 2,
            user_data: Bytes::from_static(b"ABC"),
            nsent: 1,
            ..Default::default()
        },
    );

    a.outbound_queue.abandon(10, &mut a.my_next_tsn).unwrap();

    let fwdtsn = a.create_forward_tsn();

    assert_eq!(10, fwdtsn.new_cumulative_tsn, "should be able to serialize");
    assert_eq!(1, fwdtsn.streams.len(), "there should be one stream");
    assert_eq!(1, fwdtsn.streams[0].identifier, "si should be 1");
    assert_eq!(2, fwdtsn.streams[0].sequence, "ssn should be 2");

    Ok(())
}

#[test]
fn test_create_forward_tsn_forward_two_abandoned_with_the_same_si() -> Result<()> {
    let mut a = Association {
        cumulative_tsn_ack_point: 9,
        advanced_peer_tsn_ack_point: 12,
        ..Default::default()
    };

    queue_inflight(
        &mut a,
        ChunkPayloadData {
            beginning_fragment: true,
            ending_fragment: true,
            tsn: 10,
            stream_identifier: 1,
            stream_sequence_number: 2,
            user_data: Bytes::from_static(b"ABC"),
            nsent: 1,
            ..Default::default()
        },
    );
    queue_inflight(
        &mut a,
        ChunkPayloadData {
            beginning_fragment: true,
            ending_fragment: true,
            tsn: 11,
            stream_identifier: 1,
            stream_sequence_number: 3,
            user_data: Bytes::from_static(b"DEF"),
            nsent: 1,
            ..Default::default()
        },
    );
    queue_inflight(
        &mut a,
        ChunkPayloadData {
            beginning_fragment: true,
            ending_fragment: true,
            tsn: 12,
            stream_identifier: 2,
            stream_sequence_number: 1,
            user_data: Bytes::from_static(b"123"),
            nsent: 1,
            ..Default::default()
        },
    );

    a.outbound_queue.abandon(10, &mut a.my_next_tsn).unwrap();
    a.outbound_queue.abandon(11, &mut a.my_next_tsn).unwrap();
    a.outbound_queue.abandon(12, &mut a.my_next_tsn).unwrap();

    let fwdtsn = a.create_forward_tsn();

    assert_eq!(12, fwdtsn.new_cumulative_tsn, "should be able to serialize");
    assert_eq!(2, fwdtsn.streams.len(), "there should be two stream");

    let mut si1ok = false;
    let mut si2ok = false;
    for s in &fwdtsn.streams {
        match s.identifier {
            1 => {
                assert_eq!(3, s.sequence, "ssn should be 3");
                si1ok = true;
            }
            2 => {
                assert_eq!(1, s.sequence, "ssn should be 1");
                si2ok = true;
            }
            _ => panic!("unexpected stream indentifier"),
        }
    }
    assert!(si1ok, "si=1 should be present");
    assert!(si2ok, "si=2 should be present");

    Ok(())
}

#[test]
fn test_handle_forward_tsn_forward_3unreceived_chunks() -> Result<()> {
    let mut a = Association {
        use_forward_tsn: true,
        ..Default::default()
    };

    let prev_tsn = a.peer_last_tsn;

    let fwdtsn = ChunkForwardTsn {
        new_cumulative_tsn: a.peer_last_tsn + 3,
        streams: vec![ChunkForwardTsnStream {
            identifier: 0,
            sequence: 0,
        }],
    };

    let p = a.handle_forward_tsn(&fwdtsn)?;

    let delayed_ack_triggered = a.delayed_ack_triggered;
    let immediate_ack_triggered = a.immediate_ack_triggered;
    assert_eq!(
        a.peer_last_tsn,
        prev_tsn + 3,
        "peerLastTSN should advance by 3 "
    );
    assert!(delayed_ack_triggered, "delayed sack should be triggered");
    assert!(
        !immediate_ack_triggered,
        "immediate sack should NOT be triggered"
    );
    assert!(p.is_empty(), "should return empty");

    Ok(())
}

#[test]
fn test_handle_forward_tsn_forward_1for1_missing() -> Result<()> {
    let mut a = Association {
        use_forward_tsn: true,
        ..Default::default()
    };

    let prev_tsn = a.peer_last_tsn;

    // this chunk is blocked by the missing chunk at tsn=1
    a.payload_queue.push(
        ChunkPayloadData {
            beginning_fragment: true,
            ending_fragment: true,
            tsn: a.peer_last_tsn + 2,
            stream_identifier: 0,
            stream_sequence_number: 1,
            user_data: Bytes::from_static(b"ABC"),
            ..Default::default()
        },
        a.peer_last_tsn,
    );

    let fwdtsn = ChunkForwardTsn {
        new_cumulative_tsn: a.peer_last_tsn + 1,
        streams: vec![ChunkForwardTsnStream {
            identifier: 0,
            sequence: 1,
        }],
    };

    let p = a.handle_forward_tsn(&fwdtsn)?;

    let delayed_ack_triggered = a.delayed_ack_triggered;
    let immediate_ack_triggered = a.immediate_ack_triggered;
    assert_eq!(
        a.peer_last_tsn,
        prev_tsn + 2,
        "peerLastTSN should advance by 2"
    );
    assert!(delayed_ack_triggered, "delayed sack should be triggered");
    assert!(
        !immediate_ack_triggered,
        "immediate sack should NOT be triggered"
    );
    assert!(p.is_empty(), "should return empty");

    Ok(())
}

#[test]
fn test_handle_forward_tsn_forward_1for2_missing() -> Result<()> {
    let mut a = Association {
        use_forward_tsn: true,
        ..Default::default()
    };

    a.use_forward_tsn = true;
    let prev_tsn = a.peer_last_tsn;

    // this chunk is blocked by the missing chunk at tsn=1
    a.payload_queue.push(
        ChunkPayloadData {
            beginning_fragment: true,
            ending_fragment: true,
            tsn: a.peer_last_tsn + 3,
            stream_identifier: 0,
            stream_sequence_number: 1,
            user_data: Bytes::from_static(b"ABC"),
            ..Default::default()
        },
        a.peer_last_tsn,
    );

    let fwdtsn = ChunkForwardTsn {
        new_cumulative_tsn: a.peer_last_tsn + 1,
        streams: vec![ChunkForwardTsnStream {
            identifier: 0,
            sequence: 1,
        }],
    };

    let p = a.handle_forward_tsn(&fwdtsn)?;

    let immediate_ack_triggered = a.immediate_ack_triggered;
    assert_eq!(
        a.peer_last_tsn,
        prev_tsn + 1,
        "peerLastTSN should advance by 1"
    );
    assert!(
        immediate_ack_triggered,
        "immediate sack should be triggered"
    );
    assert!(p.is_empty(), "should return empty");

    Ok(())
}

#[test]
fn test_handle_forward_tsn_dup_forward_tsn_chunk_should_generate_sack() -> Result<()> {
    let mut a = Association {
        use_forward_tsn: true,
        ..Default::default()
    };

    let prev_tsn = a.peer_last_tsn;

    let fwdtsn = ChunkForwardTsn {
        new_cumulative_tsn: a.peer_last_tsn,
        streams: vec![ChunkForwardTsnStream {
            identifier: 0,
            sequence: 1,
        }],
    };

    let p = a.handle_forward_tsn(&fwdtsn)?;

    let ack_state = a.ack_state;
    assert_eq!(a.peer_last_tsn, prev_tsn, "peerLastTSN should not advance");
    assert_eq!(AckState::Immediate, ack_state, "sack should be requested");
    assert!(p.is_empty(), "should return empty");

    Ok(())
}

fn queued_chunk(tsn: u32) -> ChunkPayloadData {
    ChunkPayloadData {
        beginning_fragment: true,
        ending_fragment: true,
        tsn,
        user_data: Bytes::from_static(b"ABC"),
        ..Default::default()
    }
}

/// A peer names `new_cumulative_tsn` freely, up to ~2^31 ahead in serial-number
/// arithmetic. Advancing one TSN per iteration turned that into a multi-second
/// hang per chunk; the cost must follow the queue, not the size of the jump.
#[test]
fn test_handle_forward_tsn_huge_gap_is_bounded() -> Result<()> {
    let mut a = Association {
        use_forward_tsn: true,
        ..Default::default()
    };
    let gap: u32 = 1 << 30;

    // One chunk the jump abandons, one beyond it that must survive.
    assert!(a.payload_queue.push(queued_chunk(5), 0));
    assert!(a.payload_queue.push(queued_chunk(gap + 5), 0));

    let start = Instant::now();
    a.handle_forward_tsn(&ChunkForwardTsn {
        new_cumulative_tsn: gap,
        streams: vec![],
    })?;
    let elapsed = start.elapsed();

    assert!(
        elapsed < Duration::from_secs(2),
        "advance must be bounded by queued data, took {elapsed:?}"
    );
    assert_eq!(a.peer_last_tsn, gap, "should reach the new cumulative TSN");
    assert!(a.payload_queue.get(5).is_none(), "abandoned chunk dropped");
    assert!(a.payload_queue.get(gap + 5).is_some(), "later chunk kept");
    assert_eq!(a.payload_queue.get_num_bytes(), 3, "bytes follow the drop");

    Ok(())
}

/// The I-FORWARD-TSN handler (RFC 8260) carries its own copy of the advance.
#[test]
fn test_handle_i_forward_tsn_huge_gap_is_bounded() -> Result<()> {
    let mut a = Association {
        use_forward_tsn: true,
        ..Default::default()
    };
    let gap: u32 = 1 << 30;

    let start = Instant::now();
    a.handle_i_forward_tsn(&ChunkIForwardTsn {
        new_cumulative_tsn: gap,
        streams: vec![],
    })?;
    let elapsed = start.elapsed();

    assert!(
        elapsed < Duration::from_secs(2),
        "advance must be bounded by queued data, took {elapsed:?}"
    );
    assert_eq!(a.peer_last_tsn, gap, "should reach the new cumulative TSN");

    Ok(())
}

#[test]
fn test_assoc_create_new_stream() -> Result<()> {
    let mut a = Association::default();

    for i in 0..ACCEPT_CH_SIZE {
        let stream_identifier =
            if let Some(s) = a.create_stream(i as u16, true, PayloadProtocolIdentifier::Unknown) {
                s.stream_identifier
            } else {
                panic!("{} should success", i);
            };
        let result = a.streams.get(&stream_identifier);
        assert!(result.is_some(), "should be in a.streams map");
    }

    let new_si = ACCEPT_CH_SIZE as u16;
    let result = a.streams.get(&new_si);
    assert!(result.is_none(), "should NOT be in a.streams map");

    let to_be_ignored = ChunkPayloadData {
        beginning_fragment: true,
        ending_fragment: true,
        tsn: a.peer_last_tsn + 1,
        stream_identifier: new_si,
        user_data: Bytes::from_static(b"ABC"),
        ..Default::default()
    };

    let p = a.handle_data(&to_be_ignored)?;
    assert!(p.is_empty(), "should return empty");

    Ok(())
}

fn handle_init_test(name: &str, initial_state: AssociationState, expect_err: bool) {
    let mut a = create_association(TransportConfig::default());
    a.set_state(initial_state);
    let pkt = Packet {
        common_header: CommonHeader {
            source_port: 5001,
            destination_port: 5002,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut init = ChunkInit {
        initial_tsn: 1234,
        num_outbound_streams: 1001,
        num_inbound_streams: 1002,
        initiate_tag: 5678,
        advertised_receiver_window_credit: 512 * 1024,
        ..Default::default()
    };
    init.set_supported_extensions();

    let result = a.handle_init(&pkt, &init);
    if expect_err {
        assert!(result.is_err(), "{} should fail", name);
        return;
    } else {
        assert!(result.is_ok(), "{} should be ok", name);
    }
    assert_eq!(
        if init.initial_tsn == 0 {
            u32::MAX
        } else {
            init.initial_tsn - 1
        },
        a.peer_last_tsn,
        "{} should match",
        name
    );
    assert_eq!(1001, a.my_max_num_outbound_streams, "{} should match", name);
    assert_eq!(1002, a.my_max_num_inbound_streams, "{} should match", name);
    assert_eq!(5678, a.peer_verification_tag, "{} should match", name);
    assert_eq!(
        pkt.common_header.source_port, a.destination_port,
        "{} should match",
        name
    );
    assert_eq!(
        pkt.common_header.destination_port, a.source_port,
        "{} should match",
        name
    );
    assert!(a.use_forward_tsn, "{} should be set to true", name);
    assert_eq!(
        512 * 1024,
        a.rwnd,
        "{} rwnd should be initialized from peer's advertised_receiver_window_credit",
        name
    );
    assert_eq!(
        a.rwnd, a.ssthresh,
        "{} ssthresh should be initialized to rwnd",
        name
    );
}

#[test]
fn test_assoc_handle_init() -> Result<()> {
    handle_init_test("normal", AssociationState::Closed, false);

    handle_init_test(
        "unexpected state established",
        AssociationState::Established,
        true,
    );

    handle_init_test(
        "unexpected state shutdownAckSent",
        AssociationState::ShutdownAckSent,
        true,
    );

    handle_init_test(
        "unexpected state shutdownPending",
        AssociationState::ShutdownPending,
        true,
    );

    handle_init_test(
        "unexpected state shutdownReceived",
        AssociationState::ShutdownReceived,
        true,
    );

    handle_init_test(
        "unexpected state shutdownSent",
        AssociationState::ShutdownSent,
        true,
    );

    Ok(())
}

#[test]
fn test_assoc_max_send_message_size_default() -> Result<()> {
    let mut a = create_association(TransportConfig::default());
    assert_eq!(65536, a.max_send_message_size, "should match");

    let ppi = PayloadProtocolIdentifier::Unknown;
    let stream = a.create_stream(1, false, ppi);
    assert!(stream.is_some(), "should succeed");

    if let Some(mut s) = stream {
        let p = Bytes::from(vec![0u8; 65537]);

        if let Err(err) = s.write_sctp(&p.slice(..65536), ppi) {
            assert_ne!(
                Error::ErrOutboundPacketTooLarge,
                err,
                "should be not Error::ErrOutboundPacketTooLarge"
            );
        } else {
            panic!("should be error");
        }

        if let Err(err) = s.write_sctp(&p.slice(..65537), ppi) {
            assert_eq!(
                Error::ErrOutboundPacketTooLarge,
                err,
                "should be Error::ErrOutboundPacketTooLarge"
            );
        } else {
            panic!("should be error");
        }
    }

    Ok(())
}

#[test]
fn test_assoc_max_send_message_size_explicit() -> Result<()> {
    let mut a = create_association(TransportConfig::default().with_max_send_message_size(30000));
    assert_eq!(30000, a.max_send_message_size, "should match");

    let ppi = PayloadProtocolIdentifier::Unknown;
    let stream = a.create_stream(1, false, ppi);
    assert!(stream.is_some(), "should succeed");

    if let Some(mut s) = stream {
        let p = Bytes::from(vec![0u8; 30001]);

        if let Err(err) = s.write_sctp(&p.slice(..30000), ppi) {
            assert_ne!(
                Error::ErrOutboundPacketTooLarge,
                err,
                "should be not Error::ErrOutboundPacketTooLarge"
            );
        } else {
            panic!("should be error");
        }

        if let Err(err) = s.write_sctp(&p.slice(..30001), ppi) {
            assert_eq!(
                Error::ErrOutboundPacketTooLarge,
                err,
                "should be Error::ErrOutboundPacketTooLarge"
            );
        } else {
            panic!("should be error");
        }
    }

    Ok(())
}

#[test]
fn test_assoc_max_receive_message_size_default() -> Result<()> {
    let mut a = create_association(TransportConfig::default());
    assert_eq!(65536, a.max_receive_message_size, "should match");

    let p = Bytes::from(vec![0u8; 65537]);

    let size_ok = ChunkPayloadData {
        beginning_fragment: true,
        ending_fragment: true,
        tsn: a.peer_last_tsn + 1,
        stream_identifier: 1,
        user_data: p.slice(..65536),
        ..Default::default()
    };

    assert!(a.handle_data(&size_ok).is_ok(), "should succeed");

    let too_large = ChunkPayloadData {
        beginning_fragment: true,
        ending_fragment: true,
        tsn: a.peer_last_tsn + 1,
        stream_identifier: 1,
        user_data: p,
        ..Default::default()
    };

    if let Err(err) = a.handle_data(&too_large) {
        assert_eq!(
            Error::ErrInboundPacketTooLarge,
            err,
            "should be Error::ErrInboundPacketTooLarge"
        );
    } else {
        panic!("should be error");
    }

    Ok(())
}

#[test]
fn test_assoc_max_receive_message_size_explicit() -> Result<()> {
    let mut a = create_association(TransportConfig::default().with_max_receive_message_size(1024));
    assert_eq!(1024, a.max_receive_message_size, "should match");

    let first_chunk = ChunkPayloadData {
        beginning_fragment: true,
        ending_fragment: true,
        tsn: a.peer_last_tsn + 1,
        stream_identifier: 1,
        user_data: Bytes::from(vec![0u8; 512]),
        ..Default::default()
    };

    assert!(a.handle_data(&first_chunk).is_ok(), "should succeed");

    let second_chunk = ChunkPayloadData {
        beginning_fragment: true,
        ending_fragment: true,
        tsn: a.peer_last_tsn + 1,
        stream_identifier: 1,
        user_data: Bytes::from(vec![0u8; 513]),
        ..Default::default()
    };

    if let Err(err) = a.handle_data(&second_chunk) {
        assert_eq!(
            Error::ErrInboundPacketTooLarge,
            err,
            "should be Error::ErrInboundPacketTooLarge"
        );
    } else {
        panic!("should be error");
    }

    Ok(())
}

#[test]
fn test_assoc_max_message_size_asymmetric() -> Result<()> {
    let config = TransportConfig::default()
        .with_max_send_message_size(1024)
        .with_max_receive_message_size(30000);

    let mut a = create_association(config);
    assert_eq!(1024, a.max_send_message_size, "should match");
    assert_eq!(30000, a.max_receive_message_size, "should match");

    let ppi = PayloadProtocolIdentifier::Unknown;
    let stream = a.create_stream(1, false, ppi);
    assert!(stream.is_some(), "should succeed");

    if let Some(mut s) = stream {
        let p = Bytes::from(vec![0u8; 1025]);

        if let Err(err) = s.write_sctp(&p.slice(..1024), ppi) {
            assert_ne!(
                Error::ErrOutboundPacketTooLarge,
                err,
                "should be not Error::ErrOutboundPacketTooLarge"
            );
        } else {
            panic!("should be error");
        }

        if let Err(err) = s.write_sctp(&p.slice(..1025), ppi) {
            assert_eq!(
                Error::ErrOutboundPacketTooLarge,
                err,
                "should be Error::ErrOutboundPacketTooLarge"
            );
        } else {
            panic!("should be error");
        }
    }

    let p = Bytes::from(vec![0u8; 30001]);

    let size_ok = ChunkPayloadData {
        beginning_fragment: true,
        ending_fragment: true,
        tsn: a.peer_last_tsn + 1,
        stream_identifier: 1,
        user_data: p.slice(..30000),
        ..Default::default()
    };

    assert!(a.handle_data(&size_ok).is_ok(), "should succeed");

    let too_large = ChunkPayloadData {
        beginning_fragment: true,
        ending_fragment: true,
        tsn: a.peer_last_tsn + 1,
        stream_identifier: 1,
        user_data: p,
        ..Default::default()
    };

    if let Err(err) = a.handle_data(&too_large) {
        assert_eq!(
            Error::ErrInboundPacketTooLarge,
            err,
            "should be Error::ErrInboundPacketTooLarge"
        );
    } else {
        panic!("should be error");
    }

    Ok(())
}

#[test]
fn test_generate_out_of_band_init() {
    let config = TransportConfig::default();
    let init_bytes = generate_snap_token(&config).unwrap();

    // Parse it back to validate
    let parsed = ChunkInit::unmarshal(&init_bytes).unwrap();

    assert!(!parsed.is_ack, "Should be INIT, not INIT ACK");
    assert!(parsed.initiate_tag != 0, "Initiate tag should not be zero");
    // Token always advertises u16::MAX for stream counts;
    // actual limits are applied from TransportConfig during negotiation.
    assert_eq!(
        parsed.num_outbound_streams,
        u16::MAX,
        "Outbound streams should always be u16::MAX in token"
    );
    assert_eq!(
        parsed.num_inbound_streams,
        u16::MAX,
        "Inbound streams should always be u16::MAX in token"
    );
    assert_eq!(
        parsed.advertised_receiver_window_credit,
        config.max_receive_buffer_size(),
        "ARWND should match config"
    );
}

#[test]
fn test_generate_out_of_band_init_with_custom_config() {
    let config = TransportConfig::default()
        .with_max_receive_buffer_size(2_000_000)
        .with_max_num_outbound_streams(256)
        .with_max_num_inbound_streams(512);

    let init_bytes = generate_snap_token(&config).unwrap();
    let parsed = ChunkInit::unmarshal(&init_bytes).unwrap();

    // Token always advertises u16::MAX for stream counts;
    // actual limits are applied from TransportConfig during negotiation.
    assert_eq!(parsed.num_outbound_streams, u16::MAX);
    assert_eq!(parsed.num_inbound_streams, u16::MAX);
    assert_eq!(parsed.advertised_receiver_window_credit, 2_000_000);
}

#[test]
fn test_generate_out_of_band_init_uniqueness() {
    // Each call to generate_snap_token creates a new INIT with unique random tags.
    let config1 = TransportConfig::default();
    let config2 = TransportConfig::default();

    let init1 = generate_snap_token(&config1).unwrap();
    let init2 = generate_snap_token(&config2).unwrap();

    let parsed1 = ChunkInit::unmarshal(&init1).unwrap();
    let parsed2 = ChunkInit::unmarshal(&init2).unwrap();

    // Initiate tags should be different (random)
    assert_ne!(
        parsed1.initiate_tag, parsed2.initiate_tag,
        "Initiate tags should be unique across calls"
    );

    // Initial TSNs should be different (random)
    assert_ne!(
        parsed1.initial_tsn, parsed2.initial_tsn,
        "Initial TSNs should be unique across calls"
    );
}

#[test]
fn test_out_of_band_association_creation() {
    let local_config = Arc::new(TransportConfig::default());
    let remote_config = TransportConfig::default();
    let max_payload_size = 1200;

    let local_init_bytes = generate_snap_token(&local_config).unwrap();
    let remote_init_bytes = generate_snap_token(&remote_config).unwrap();

    let local_init = ChunkInit::unmarshal(&local_init_bytes).unwrap();
    let remote_init = ChunkInit::unmarshal(&remote_init_bytes).unwrap();

    let remote_addr: SocketAddr = "192.168.1.1:5000".parse().unwrap();

    let assoc = Association::new_with_out_of_band_init(
        local_config.clone(),
        max_payload_size,
        remote_addr,
        None,
        local_init.clone(),
        remote_init.clone(),
    )
    .expect("Should create out-of-band init association");

    // Verify the association is in ESTABLISHED state
    assert_eq!(
        assoc.state(),
        AssociationState::Established,
        "Out-of-band init association should be in ESTABLISHED state"
    );

    // Verify handshake is marked complete
    assert!(
        assoc.handshake_completed,
        "Out-of-band init association should have handshake completed"
    );

    // Verify verification tags
    assert_eq!(
        assoc.my_verification_tag, local_init.initiate_tag,
        "My verification tag should match local init"
    );
    assert_eq!(
        assoc.peer_verification_tag, remote_init.initiate_tag,
        "Peer verification tag should match remote init"
    );

    // Verify TSN setup
    assert_eq!(
        assoc.my_next_tsn, local_init.initial_tsn,
        "My next TSN should match local init"
    );
    assert_eq!(
        assoc.peer_last_tsn,
        remote_init.initial_tsn.wrapping_sub(1),
        "Peer last TSN should be remote init TSN - 1"
    );

    // Verify rwnd
    assert_eq!(
        assoc.rwnd, remote_init.advertised_receiver_window_credit,
        "rwnd should match remote advertised credit"
    );
}

#[test]
fn test_out_of_band_association_stream_negotiation() {
    let config = Arc::new(
        TransportConfig::default()
            .with_max_num_outbound_streams(100)
            .with_max_num_inbound_streams(200),
    );

    // Remote uses default config — token always advertises u16::MAX
    let remote_config = TransportConfig::default();

    let local_init_bytes = generate_snap_token(&config).unwrap();
    let remote_init_bytes = generate_snap_token(&remote_config).unwrap();

    let local_init = ChunkInit::unmarshal(&local_init_bytes).unwrap();
    let remote_init = ChunkInit::unmarshal(&remote_init_bytes).unwrap();

    // Token always advertises u16::MAX for stream counts
    assert_eq!(local_init.num_outbound_streams, u16::MAX);
    assert_eq!(local_init.num_inbound_streams, u16::MAX);
    assert_eq!(remote_init.num_outbound_streams, u16::MAX);
    assert_eq!(remote_init.num_inbound_streams, u16::MAX);

    let remote_addr: SocketAddr = "192.168.1.1:5000".parse().unwrap();

    let assoc = Association::new_with_out_of_band_init(
        config.clone(),
        1200,
        remote_addr,
        None,
        local_init,
        remote_init,
    )
    .expect("Should create out-of-band init association");

    // Stream limits should be clamped by the local config, since the
    // remote token always offers u16::MAX.
    // my_max_num_outbound_streams = min(config.max_out=100, remote_in=MAX) = 100
    assert_eq!(
        assoc.my_max_num_outbound_streams, 100,
        "Outbound streams should be clamped by local config"
    );

    // my_max_num_inbound_streams = min(config.max_in=200, remote_out=MAX) = 200
    assert_eq!(
        assoc.my_max_num_inbound_streams, 200,
        "Inbound streams should be clamped by local config"
    );
}

#[test]
fn test_out_of_band_connected_event() {
    let local_config = Arc::new(TransportConfig::default());
    let remote_config = TransportConfig::default();

    let local_init_bytes = generate_snap_token(&local_config).unwrap();
    let remote_init_bytes = generate_snap_token(&remote_config).unwrap();

    let local_init = ChunkInit::unmarshal(&local_init_bytes).unwrap();
    let remote_init = ChunkInit::unmarshal(&remote_init_bytes).unwrap();

    let remote_addr: SocketAddr = "192.168.1.1:5000".parse().unwrap();

    let mut assoc = Association::new_with_out_of_band_init(
        local_config.clone(),
        1200,
        remote_addr,
        None,
        local_init,
        remote_init,
    )
    .expect("Should create out-of-band init association");

    // Poll should return a Connected event
    let event = assoc.poll();
    assert!(
        matches!(event, Some(Event::Connected)),
        "Should emit Connected event, got {:?}",
        event
    );
}

#[test]
fn test_out_of_band_symmetric_setup() {
    // Test that both sides of an out-of-band init association work correctly
    let config_a = Arc::new(TransportConfig::default());
    let config_b = Arc::new(TransportConfig::default());

    let init_a_bytes = generate_snap_token(&config_a).unwrap();
    let init_b_bytes = generate_snap_token(&config_b).unwrap();

    let init_a = ChunkInit::unmarshal(&init_a_bytes).unwrap();
    let init_b = ChunkInit::unmarshal(&init_b_bytes).unwrap();

    let addr_a: SocketAddr = "192.168.1.1:5000".parse().unwrap();
    let addr_b: SocketAddr = "192.168.1.2:5000".parse().unwrap();

    // Create association A (local=A, remote=B)
    let assoc_a = Association::new_with_out_of_band_init(
        config_a.clone(),
        1200,
        addr_b,
        None,
        init_a.clone(),
        init_b.clone(),
    )
    .expect("Should create association A");

    // Create association B (local=B, remote=A)
    let assoc_b = Association::new_with_out_of_band_init(
        config_b.clone(),
        1200,
        addr_a,
        None,
        init_b.clone(),
        init_a.clone(),
    )
    .expect("Should create association B");

    // Verify both are in ESTABLISHED state
    assert_eq!(assoc_a.state(), AssociationState::Established);
    assert_eq!(assoc_b.state(), AssociationState::Established);

    // Verify verification tags are cross-matched
    assert_eq!(assoc_a.my_verification_tag, assoc_b.peer_verification_tag);
    assert_eq!(assoc_b.my_verification_tag, assoc_a.peer_verification_tag);
}

#[test]
fn test_out_of_band_with_forward_tsn_support() {
    let local_config = Arc::new(TransportConfig::default());
    let remote_config = TransportConfig::default();

    let local_init_bytes = generate_snap_token(&local_config).unwrap();
    let remote_init_bytes = generate_snap_token(&remote_config).unwrap();

    let local_init = ChunkInit::unmarshal(&local_init_bytes).unwrap();
    let remote_init = ChunkInit::unmarshal(&remote_init_bytes).unwrap();

    // Verify supported extensions are present
    let mut has_forward_tsn = false;
    for param in &local_init.params {
        if let Some(ext) = param
            .as_any()
            .downcast_ref::<crate::param::param_supported_extensions::ParamSupportedExtensions>(
        ) {
            for ct in &ext.chunk_types {
                if *ct == crate::chunk::chunk_type::CT_FORWARD_TSN {
                    has_forward_tsn = true;
                }
            }
        }
    }
    assert!(
        has_forward_tsn,
        "Generated INIT should include ForwardTSN support"
    );

    let remote_addr: SocketAddr = "192.168.1.1:5000".parse().unwrap();

    let assoc = Association::new_with_out_of_band_init(
        local_config.clone(),
        1200,
        remote_addr,
        None,
        local_init,
        remote_init,
    )
    .expect("Should create out-of-band init association");

    assert!(
        assoc.use_forward_tsn,
        "Out-of-band init association should have ForwardTSN enabled"
    );
}

#[test]
fn test_out_of_band_initial_tsn_zero_wrap() {
    // Test edge case where initial TSN is 0 (wraps to MAX)
    let config = Arc::new(TransportConfig::default());

    let local_init_bytes = generate_snap_token(&config).unwrap();
    let mut remote_init = ChunkInit::unmarshal(&local_init_bytes).unwrap();

    // Set initial TSN to 0 to test the edge case
    remote_init.initial_tsn = 0;
    remote_init.initiate_tag = 12345;

    // Generate a fresh local init for the association
    let actual_local_init_bytes = generate_snap_token(&config).unwrap();
    let local_init = ChunkInit::unmarshal(&actual_local_init_bytes).unwrap();
    let remote_addr: SocketAddr = "192.168.1.1:5000".parse().unwrap();

    let assoc = Association::new_with_out_of_band_init(
        config.clone(),
        1200,
        remote_addr,
        None,
        local_init,
        remote_init,
    )
    .expect("Should create out-of-band init association");

    // peer_last_tsn should be u32::MAX when initial_tsn is 0
    assert_eq!(
        assoc.peer_last_tsn,
        u32::MAX,
        "peer_last_tsn should wrap to MAX when initial_tsn is 0"
    );
}

#[test]
fn test_out_of_band_rwnd_negotiation() {
    let local_config = Arc::new(TransportConfig::default().with_max_receive_buffer_size(500_000));

    let remote_config = TransportConfig::default().with_max_receive_buffer_size(300_000);

    let local_init_bytes = generate_snap_token(&local_config).unwrap();
    let remote_init_bytes = generate_snap_token(&remote_config).unwrap();

    let local_init = ChunkInit::unmarshal(&local_init_bytes).unwrap();
    let remote_init = ChunkInit::unmarshal(&remote_init_bytes).unwrap();

    let remote_addr: SocketAddr = "192.168.1.1:5000".parse().unwrap();

    let assoc = Association::new_with_out_of_band_init(
        local_config.clone(),
        1200,
        remote_addr,
        None,
        local_init,
        remote_init,
    )
    .expect("Should create out-of-band init association");

    // rwnd should be set to remote's advertised receiver window credit
    assert_eq!(
        assoc.rwnd, 300_000,
        "rwnd should be remote's advertised window"
    );
}

#[test]
fn test_initial_cwnd_small_mtu() {
    // Regression test for #47: a small MTU made 4*MTU < 4380, which previously
    // panicked because clamp(4380, 4*MTU) was called with min > max.
    let max_payload_size = 100;
    let mtu = max_payload_size + COMMON_HEADER_SIZE + DATA_CHUNK_HEADER_SIZE;
    assert!(4 * mtu < 4380, "test must exercise the small-MTU branch");

    let assoc = Association::new(
        None,
        Arc::new(TransportConfig::default()),
        max_payload_size,
        0,
        SocketAddr::from_str("0.0.0.0:0").unwrap(),
        None,
        Instant::now(),
    );

    // RFC 4960 Sec 7.2.1: min(4*MTU, max(2*MTU, 4380)).
    assert_eq!(assoc.cwnd, (4 * mtu).min((2 * mtu).max(4380)));
    assert_eq!(assoc.cwnd, 4 * mtu);
}

fn first_transmit_common_header(a: &mut Association) -> CommonHeader {
    let transmit = a
        .poll_transmit(Instant::now())
        .expect("client association should emit an INIT");

    let raw = match transmit.payload {
        Payload::RawEncode(ref raws) => raws.first().expect("at least one packet").clone(),
        _ => panic!("outbound transmit should encode"),
    };

    Packet::unmarshal(&raw)
        .expect("INIT should unmarshal")
        .common_header
}

#[test]
fn test_client_uses_configured_sctp_ports() {
    let mut a = create_association(
        TransportConfig::default()
            .with_local_port(41234)
            .with_remote_port(9899),
    );

    assert_eq!(41234, a.source_port);
    assert_eq!(9899, a.destination_port);

    let header = first_transmit_common_header(&mut a);
    assert_eq!(41234, header.source_port, "INIT source port on the wire");
    assert_eq!(
        9899, header.destination_port,
        "INIT destination port on the wire"
    );
}

#[test]
fn test_client_defaults_to_well_known_sctp_port() {
    let mut a = create_association(TransportConfig::default());
    assert_eq!(DEFAULT_SCTP_PORT, a.source_port);
    assert_eq!(DEFAULT_SCTP_PORT, a.destination_port);

    let header = first_transmit_common_header(&mut a);
    assert_eq!(DEFAULT_SCTP_PORT, header.source_port);
    assert_eq!(DEFAULT_SCTP_PORT, header.destination_port);
}

#[test]
fn test_out_of_band_init_uses_configured_sctp_ports() {
    let local_init = ChunkInit {
        initiate_tag: 1,
        ..Default::default()
    };
    let remote_init = ChunkInit {
        initiate_tag: 2,
        ..Default::default()
    };
    let a = Association::new_with_out_of_band_init(
        Arc::new(
            TransportConfig::default()
                .with_local_port(6001)
                .with_remote_port(6002),
        ),
        1400,
        SocketAddr::from_str("0.0.0.0:0").unwrap(),
        None,
        local_init,
        remote_init,
    )
    .expect("out-of-band association");

    assert_eq!(6001, a.source_port);
    assert_eq!(6002, a.destination_port);
}

fn receive_capacity_fragment(tsn: u32, first: bool, last: bool) -> ChunkPayloadData {
    ChunkPayloadData {
        tsn,
        stream_identifier: 1,
        beginning_fragment: first,
        ending_fragment: last,
        user_data: Bytes::from(vec![0; 1024]),
        ..Default::default()
    }
}

fn receive_capacity_datagram(a: &mut Association, chunk: ChunkPayloadData) -> Result<()> {
    let packet = a.create_packet(vec![Box::new(chunk)]).marshal()?;
    a.handle_event(AssociationEvent(AssociationEventInner::Datagram(
        Transmit {
            now: Instant::now(),
            remote: a.remote_addr(),
            ecn: None,
            local_ip: None,
            payload: Payload::PartialDecode(crate::packet::PartialDecode::unmarshal(&packet)?),
        },
    )));
    Ok(())
}

#[test]
fn test_receive_capacity_reports_undrainable_message() -> Result<()> {
    for unordered in [false, true] {
        for initial_tsn in [0u32, u32::MAX - 16] {
            let mut a = create_association(
                TransportConfig::default().with_max_receive_buffer_size(32 * 1024),
            );
            a.set_state(AssociationState::Established);
            a.peer_last_tsn = initial_tsn;
            for index in 1..=32 {
                let mut fragment =
                    receive_capacity_fragment(initial_tsn.wrapping_add(index), index == 1, false);
                fragment.unordered = unordered;
                receive_capacity_datagram(&mut a, fragment)?;
                if index < 32 {
                    assert!(!a.is_closed());
                    assert_eq!(a.get_my_receiver_window_credit(), (32 - index) * 1024);
                    assert!(a.stream(1)?.read()?.is_none());
                }
            }
            assert!(a.is_closed());
            assert!(core::iter::from_fn(|| a.poll()).any(|event| matches!(
                event,
                Event::AssociationLost {
                    reason: AssociationError::ReceiveBufferExhausted
                }
            )));
            let packets = a.gather_outbound(Instant::now()).0;
            assert_eq!(packets.len(), 1);
            let packet = crate::packet::PartialDecode::unmarshal(&packets[0])?.finish()?;
            let abort = packet.chunks[0]
                .as_any()
                .downcast_ref::<ChunkAbort>()
                .unwrap();
            assert_eq!(abort.error_causes[0].code, crate::chunk::OUT_OF_RESOURCE);
            // Closing actually releases storage, rather than inventing credit.
            assert_eq!(a.get_my_receiver_window_credit(), 32 * 1024);
        }
    }
    Ok(())
}

#[test]
fn test_receive_capacity_zero_window_preserves_backpressure() -> Result<()> {
    let mut a =
        create_association(TransportConfig::default().with_max_receive_buffer_size(32 * 1024));
    a.set_state(AssociationState::Established);
    a.peer_last_tsn = 0;
    for tsn in 1..=32 {
        a.handle_data(&receive_capacity_fragment(tsn, tsn == 1, tsn == 32))?;
    }
    assert_eq!(a.get_my_receiver_window_credit(), 0);
    a.ack_state = AckState::Immediate;
    let packets = a.gather_outbound_sack_packets(vec![]);
    let packet = crate::packet::PartialDecode::unmarshal(&packets[0])?.finish()?;
    let sack = packet.chunks[0]
        .as_any()
        .downcast_ref::<ChunkSelectiveAck>()
        .unwrap();
    assert_eq!(sack.advertised_receiver_window_credit, 0);
    assert_eq!(sack.cumulative_tsn_ack, 32);
    let mut next = receive_capacity_fragment(33, true, true);
    next.stream_identifier = 2;
    for _ in 0..3 {
        a.handle_data(&next)?;
        assert_eq!(a.peer_last_tsn, 32);
        assert_eq!(a.get_my_receiver_window_credit(), 0);
        assert!(!a.is_closed());
    }
    assert!(a.stream(1)?.read()?.is_some());
    assert_eq!(a.get_my_receiver_window_credit(), 32 * 1024);
    a.handle_data(&next)?;
    assert!(a.stream(2)?.read()?.is_some());
    assert_eq!(a.get_my_receiver_window_credit(), 32 * 1024);
    Ok(())
}

#[test]
fn test_receive_capacity_gap_fill_and_duplicates() -> Result<()> {
    let mut a =
        create_association(TransportConfig::default().with_max_receive_buffer_size(32 * 1024));
    a.peer_last_tsn = u32::MAX - 16;
    let initial_tsn = a.peer_last_tsn;
    // Lose the beginning fragment and reorder the ending fragment. Accepted
    // duplicates do not consume extra storage or turn gap recovery into failure.
    for index in (2..=33).rev() {
        let chunk = receive_capacity_fragment(initial_tsn.wrapping_add(index), false, index == 33);
        a.handle_data(&chunk)?;
        let credit = a.get_my_receiver_window_credit();
        a.handle_data(&chunk)?;
        assert_eq!(a.get_my_receiver_window_credit(), credit);
    }
    assert_eq!(a.get_my_receiver_window_credit(), 0);
    a.handle_data(&receive_capacity_fragment(
        initial_tsn.wrapping_add(1),
        true,
        false,
    ))?;
    assert_eq!(a.peer_last_tsn, initial_tsn.wrapping_add(33));
    assert_eq!(a.get_my_receiver_window_credit(), 0);
    assert!(a.stream(1)?.read()?.is_some());
    assert_eq!(a.get_my_receiver_window_credit(), 32 * 1024);
    Ok(())
}

#[test]
fn test_receive_capacity_shared_by_incomplete_streams() -> Result<()> {
    let mut a =
        create_association(TransportConfig::default().with_max_receive_buffer_size(32 * 1024));
    a.set_state(AssociationState::Established);
    a.peer_last_tsn = 0;
    for tsn in 1..=32 {
        let mut chunk = receive_capacity_fragment(tsn, tsn <= 2, false);
        chunk.stream_identifier = (tsn % 2) as u16;
        chunk.unordered = true;
        receive_capacity_datagram(&mut a, chunk)?;
    }
    assert!(a.is_closed());
    assert!(core::iter::from_fn(|| a.poll()).any(|event| matches!(
        event,
        Event::AssociationLost {
            reason: AssociationError::ReceiveBufferExhausted
        }
    )));
    Ok(())
}

#[test]
fn test_receive_capacity_counts_retiring_and_deferred_data() -> Result<()> {
    let mut a = association_with_retiring_boundary_data()?;
    a.max_receive_buffer_size = 32 * 1024;
    a.max_receive_message_size = 64 * 1024;
    let mut successor = receive_capacity_fragment(2, true, false);
    successor.user_data = Bytes::from(vec![0; 32 * 1024 - 3]);
    a.handle_data(&successor)?;
    assert_eq!(a.get_my_receiver_window_credit(), 0);
    assert!(!a.deferred_reset_data.is_empty());
    assert!(!a.is_closed());
    // Reading the retiring generation releases only its three bytes. Moving
    // the successor out of deferred storage must not create additional credit.
    assert!(a.stream(1)?.read()?.is_some());
    assert_eq!(a.get_my_receiver_window_credit(), 3);
    assert!(a.deferred_reset_data.is_empty());
    assert!(a.stream(1)?.read()?.is_none());
    let mut tail = receive_capacity_fragment(3, false, false);
    tail.user_data = Bytes::from_static(b"new");
    assert_eq!(
        a.handle_data(&tail).unwrap_err(),
        Error::ErrReceiveBufferExhausted
    );
    assert_eq!(a.get_my_receiver_window_credit(), 0);
    Ok(())
}

#[test]
fn test_receive_capacity_detected_after_reading_last_complete_message() -> Result<()> {
    let mut a =
        create_association(TransportConfig::default().with_max_receive_buffer_size(32 * 1024));
    a.set_state(AssociationState::Established);
    a.peer_last_tsn = 0;
    let mut complete = receive_capacity_fragment(1, true, true);
    complete.payload_type = PayloadProtocolIdentifier::Binary;
    complete.user_data = Bytes::from_static(b"old");
    a.handle_data(&complete)?;
    for tsn in 2..=33 {
        let mut partial = receive_capacity_fragment(tsn, tsn == 2, false);
        partial.stream_identifier = 2;
        a.handle_data(&partial)?;
    }
    assert_eq!(a.get_my_receiver_window_credit(), 0);
    assert!(!a.is_closed());
    assert!(a.stream(1)?.read()?.is_some());
    assert!(a.is_closed());
    let events: Vec<_> = core::iter::from_fn(|| a.poll()).collect();
    let final_stats = events
        .iter()
        .find_map(|event| match event {
            Event::DataChannelStatsFinal(stats) if stats.stream_identifier == 1 => Some(stats),
            _ => None,
        })
        .expect("receive-capacity closure preserves final stats");
    assert_eq!(
        (final_stats.messages_received, final_stats.bytes_received),
        (1, 3)
    );
    assert!(events.iter().any(|event| matches!(
        event,
        Event::AssociationLost {
            reason: AssociationError::ReceiveBufferExhausted
        }
    )));
    Ok(())
}

#[test]
fn test_receive_capacity_reports_exhaustion_after_forward_tsn_gap_closure() -> Result<()> {
    let mut a =
        create_association(TransportConfig::default().with_max_receive_buffer_size(32 * 1024));
    a.set_state(AssociationState::Established);
    a.use_forward_tsn = true;
    a.peer_last_tsn = 0;
    // TSN 1 is an abandoned message on stream 2. Stream 1's reliable message
    // fills the receive budget while that missing TSN keeps the gap queue alive.
    for tsn in 2..=33 {
        receive_capacity_datagram(&mut a, receive_capacity_fragment(tsn, tsn == 2, false))?;
    }
    assert!(!a.is_closed());
    assert_eq!(a.get_my_receiver_window_credit(), 0);
    assert!(a.stream(1)?.read()?.is_none());
    let forward = ChunkForwardTsn {
        new_cumulative_tsn: 1,
        streams: vec![ChunkForwardTsnStream {
            identifier: 2,
            sequence: 0,
        }],
    };
    let packet = a.create_packet(vec![Box::new(forward)]).marshal()?;
    a.handle_event(AssociationEvent(AssociationEventInner::Datagram(
        Transmit {
            now: Instant::now(),
            remote: a.remote_addr(),
            ecn: None,
            local_ip: None,
            payload: Payload::PartialDecode(crate::packet::PartialDecode::unmarshal(&packet)?),
        },
    )));
    assert!(
        a.is_closed(),
        "FORWARD-TSN must reject the now-undrainable receive buffer"
    );
    assert!(core::iter::from_fn(|| a.poll()).any(|event| matches!(
        event,
        Event::AssociationLost {
            reason: AssociationError::ReceiveBufferExhausted
        }
    )));
    Ok(())
}

#[test]
fn test_receive_capacity_stopped_stream_cannot_supply_backpressure() -> Result<()> {
    let mut a =
        create_association(TransportConfig::default().with_max_receive_buffer_size(32 * 1024));
    a.set_state(AssociationState::Established);
    a.peer_last_tsn = 0;
    receive_capacity_datagram(&mut a, receive_capacity_fragment(1, true, true))?;
    a.stream(1)?.stop()?;
    assert_eq!(a.stream(1)?.read().unwrap_err(), Error::ErrStreamClosed);
    // A completed message on the stopped read half cannot release storage via
    // the public API. The independent reliable message fills the remaining
    // receive budget, and its ending fragment would be a new highest TSN.
    for tsn in 2..=32 {
        let mut chunk = receive_capacity_fragment(tsn, tsn == 2, false);
        chunk.stream_identifier = 2;
        receive_capacity_datagram(&mut a, chunk)?;
    }
    if !a.is_closed() {
        assert_eq!(a.get_my_receiver_window_credit(), 0);
        assert!(a.stream(2)?.read()?.is_none());
        // The peer can have assigned its last TSN to the ending fragment
        // before answering the reset. That reset cannot free the stopped
        // stream until this new highest TSN is received.
        let mut replies = vec![];
        a.reset_streams_if_any(
            &ParamOutgoingResetRequest {
                reconfig_request_sequence_number: 1,
                reconfig_response_sequence_number: 0,
                sender_last_tsn: 33,
                stream_identifiers: vec![1],
            },
            true,
            &mut replies,
        )?;
        let mut ending = receive_capacity_fragment(33, false, true);
        ending.stream_identifier = 2;
        for _ in 0..3 {
            receive_capacity_datagram(&mut a, ending.clone())?;
            assert_eq!(a.peer_last_tsn, 32);
            assert_eq!(a.get_my_receiver_window_credit(), 0);
        }
    }
    assert!(
        a.is_closed(),
        "a stopped read half cannot make receive storage drainable"
    );
    assert!(core::iter::from_fn(|| a.poll()).any(|event| matches!(
        event,
        Event::AssociationLost {
            reason: AssociationError::ReceiveBufferExhausted
        }
    )));
    Ok(())
}

#[test]
fn test_receive_capacity_detected_when_last_readable_stream_stops() -> Result<()> {
    let mut a =
        create_association(TransportConfig::default().with_max_receive_buffer_size(32 * 1024));
    a.set_state(AssociationState::Established);
    a.peer_last_tsn = 0;
    receive_capacity_datagram(&mut a, receive_capacity_fragment(1, true, true))?;
    for tsn in 2..=32 {
        let mut chunk = receive_capacity_fragment(tsn, tsn == 2, false);
        chunk.stream_identifier = 2;
        receive_capacity_datagram(&mut a, chunk)?;
    }
    assert!(!a.is_closed());
    assert_eq!(a.get_my_receiver_window_credit(), 0);
    a.stream(1)?.stop()?;
    assert!(
        a.is_closed(),
        "stop must detect storage the API can no longer drain"
    );
    assert!(core::iter::from_fn(|| a.poll()).any(|event| matches!(
        event,
        Event::AssociationLost {
            reason: AssociationError::ReceiveBufferExhausted
        }
    )));
    Ok(())
}

#[test]
fn test_data_channel_stats_acceptance_and_delivery() -> Result<()> {
    let mut a = create_association(TransportConfig::default());
    a.set_state(AssociationState::Established);
    let ppi = PayloadProtocolIdentifier::Binary;
    let mut stream = a.create_stream(12, false, ppi).unwrap();
    let initial = stream.stats()?;
    assert_eq!(initial.messages_sent, 0);
    assert_eq!(initial, stream.stats()?);
    stream.write(b"abc")?;
    stream.write_chunk(&Bytes::from_static(b"xy"))?;
    stream.write_sctp(&Bytes::from_static(b"z"), ppi)?;
    stream.write_with_ppi("é".as_bytes(), PayloadProtocolIdentifier::String)?;
    stream.write_with_ppi(&[0], PayloadProtocolIdentifier::BinaryEmpty)?;
    stream.write_with_ppi(&[0], PayloadProtocolIdentifier::StringEmpty)?;
    stream.write_with_ppi(b"control", PayloadProtocolIdentifier::Dcep)?;
    stream.write_with_ppi(b"unknown", PayloadProtocolIdentifier::Unknown)?;
    stream.write(b"")?;
    let mut buffers = [
        Bytes::new(),
        Bytes::from_static(b"first"),
        Bytes::from_static(b"second"),
    ];
    assert_eq!(stream.write_chunks(&mut buffers)?, 5);
    assert_eq!(buffers[2], b"second"[..]);
    let stats = stream.stats()?;
    assert_eq!((stats.messages_sent, stats.bytes_sent), (7, 13));
    assert!(stream.write(&vec![0; 1_000_000]).is_err());
    assert_eq!(stats, stream.stats()?);

    // Fragmentation, reordering and duplicates are accounted only at handoff.
    let data = Bytes::from(vec![42; 3000]);
    stream.write_sctp(&data, ppi)?;
    let before_read = stream.stats()?;
    let receiver = a.streams.get_mut(&12).unwrap();
    let mut fragments = vec![];
    for (i, part) in data.chunks(1000).enumerate() {
        fragments.push(ChunkPayloadData {
            tsn: i as u32 + 1,
            stream_identifier: 12,
            stream_sequence_number: 0,
            beginning_fragment: i == 0,
            ending_fragment: i == 2,
            payload_type: ppi,
            user_data: Bytes::copy_from_slice(part),
            ..Default::default()
        });
    }
    receiver.handle_data(&fragments[2])?;
    receiver.handle_data(&fragments[0])?;
    receiver.handle_data(&fragments[0])?;
    assert!(a.stream(12).unwrap().read()?.is_none());
    a.streams.get_mut(&12).unwrap().handle_data(&fragments[1])?;
    let mut stream = a.stream(12).unwrap();
    let mut message = stream.read()?.unwrap();
    assert_eq!(message.next(1).unwrap().bytes.len(), 1);
    let mut output = vec![0; 3000];
    message.clone().read(&mut output)?;
    message.read(&mut output)?;
    assert!(stream.read()?.is_none());
    let stats = stream.stats()?;
    assert_eq!((stats.messages_received, stats.bytes_received), (1, 3000));
    assert_eq!(stats.messages_sent, before_read.messages_sent);
    stream.finish()?;
    assert!(stream.write(b"rejected").is_err());
    assert_eq!(stats, stream.stats()?);
    Ok(())
}

#[test]
fn test_data_channel_stats_retirement_reuse_and_loss() -> Result<()> {
    let mut a = create_association(TransportConfig::default());
    a.set_state(AssociationState::Established);
    let initial = a
        .create_stream(4, false, PayloadProtocolIdentifier::Binary)
        .unwrap()
        .stats()?;
    a.streams
        .get_mut(&4)
        .unwrap()
        .handle_data(&ChunkPayloadData {
            tsn: 1,
            stream_identifier: 4,
            beginning_fragment: true,
            ending_fragment: true,
            payload_type: PayloadProtocolIdentifier::StringEmpty,
            user_data: Bytes::from_static(&[0]),
            ..Default::default()
        })?;
    a.retire_stream(4, 1);
    assert!(a.stream(4).unwrap().read()?.is_some());
    let mut finals = vec![];
    while let Some(event) = a.poll() {
        if let Event::DataChannelStatsFinal(stats) = event {
            finals.push(stats);
        }
    }
    assert_eq!(finals.len(), 1);
    assert_eq!(finals[0].generation, initial.generation);
    assert_eq!(
        (finals[0].messages_received, finals[0].bytes_received),
        (1, 0)
    );
    let fresh = a
        .create_stream(4, false, PayloadProtocolIdentifier::Binary)
        .unwrap()
        .stats()?;
    assert_ne!(fresh.generation, initial.generation);
    assert_eq!(fresh.messages_received, 0);
    a.close()?;
    a.close()?;
    while let Some(event) = a.poll() {
        if let Event::DataChannelStatsFinal(stats) = event {
            finals.push(stats);
        }
    }
    assert_eq!(finals.len(), 2);
    assert_eq!(finals[1], fresh);
    Ok(())
}

#[test]
fn test_data_channel_stats_receive_projection_and_isolation() -> Result<()> {
    let mut a = create_association(TransportConfig::default());
    a.set_state(AssociationState::Established);
    a.create_stream(1, false, PayloadProtocolIdentifier::Binary)
        .unwrap();
    let untouched = a
        .create_stream(2, false, PayloadProtocolIdentifier::Binary)
        .unwrap()
        .stats()?;
    let mut other = create_association(TransportConfig::default());
    let separate = other
        .create_stream(1, false, PayloadProtocolIdentifier::Binary)
        .unwrap()
        .stats()?;
    let payloads = [
        (PayloadProtocolIdentifier::Binary, &b"abc"[..]),
        (PayloadProtocolIdentifier::String, "é".as_bytes()),
        (PayloadProtocolIdentifier::StringEmpty, &b"\0"[..]),
        (PayloadProtocolIdentifier::BinaryEmpty, &b"\0"[..]),
        (PayloadProtocolIdentifier::Dcep, &b"control"[..]),
        (PayloadProtocolIdentifier::Unknown, &b"unknown"[..]),
    ];
    for (i, (ppi, data)) in payloads.into_iter().enumerate() {
        a.streams
            .get_mut(&1)
            .unwrap()
            .handle_data(&ChunkPayloadData {
                tsn: i as u32 + 1,
                stream_sequence_number: i as u16,
                stream_identifier: 1,
                beginning_fragment: true,
                ending_fragment: true,
                payload_type: ppi,
                user_data: Bytes::copy_from_slice(data),
                ..Default::default()
            })?;
        a.stream(1)?.read()?.unwrap();
    }
    let stats = a.stream(1)?.stats()?;
    assert_eq!((stats.messages_received, stats.bytes_received), (4, 5));
    assert_eq!(a.stream(2)?.stats()?, untouched);
    assert_eq!(other.stream(1)?.stats()?, separate);
    // A closed receive half rejects reads without changing cumulative totals.
    a.stream(1)?.stop()?;
    assert!(a.stream(1)?.read().is_err());
    assert_eq!(a.stream(1)?.stats()?, stats);
    Ok(())
}

#[test]
fn test_data_channel_stats_failed_queue_and_abandoned_receive() -> Result<()> {
    let mut a = create_association(TransportConfig::default());
    let before = a
        .create_stream(1, false, PayloadProtocolIdentifier::Binary)
        .unwrap()
        .stats()?;
    // Queueing rejects a write before the association is established.
    assert!(a.stream(1)?.write(b"not accepted").is_err());
    assert_eq!(a.stream(1)?.stats()?, before);
    a.set_state(AssociationState::Established);
    a.streams
        .get_mut(&1)
        .unwrap()
        .handle_data(&ChunkPayloadData {
            tsn: 1,
            stream_identifier: 1,
            beginning_fragment: true,
            ending_fragment: false,
            payload_type: PayloadProtocolIdentifier::Binary,
            user_data: Bytes::from_static(b"incomplete"),
            ..Default::default()
        })?;
    assert!(a.stream(1)?.read()?.is_none());
    a.streams
        .get_mut(&1)
        .unwrap()
        .reassembly_queue
        .forward_tsn_for_ordered(0);
    assert!(a.stream(1)?.read()?.is_none());
    assert_eq!(a.stream(1)?.stats()?, before);
    a.next_stream_generation = u64::MAX;
    assert!(
        a.create_stream(2, false, PayloadProtocolIdentifier::Binary)
            .is_none()
    );
    assert_eq!(a.stream(1)?.stats()?, before);
    Ok(())
}

fn queue_pending(a: &mut Association, chunk: ChunkPayloadData) {
    a.outbound_queue.push(vec![chunk]);
}

fn queue_inflight(a: &mut Association, mut chunk: ChunkPayloadData) {
    // These fixtures insert one complete message through the real scheduler.
    chunk.beginning_fragment = true;
    chunk.ending_fragment = true;
    let tsn = chunk.tsn;
    queue_pending(a, chunk);
    a.outbound_queue
        .send_next(&mut { tsn }, Instant::now())
        .unwrap();
}

fn fragmented_retransmission_fixture(limit: u32) -> Association {
    let mut a = Association {
        state: AssociationState::Established,
        use_forward_tsn: true,
        my_next_tsn: 1,
        max_send_message_size: 100,
        max_receive_message_size: 100,
        max_payload_size: 4,
        cwnd: 100,
        rwnd: 100,
        mtu: 1400,
        ..Default::default()
    };
    a.create_stream(1, false, PayloadProtocolIdentifier::Binary)
        .unwrap();
    let mut stream = a.stream(1).unwrap();
    stream
        .set_reliability_params(false, ReliabilityType::Rexmit, limit)
        .unwrap();
    stream.write(b"abcdefghij").unwrap();
    a
}

#[test]
fn test_fast_retransmission_abandons_message_and_pending_tail() {
    let now = Instant::now();
    let mut a = fragmented_retransmission_fixture(0);
    a.send_next_data_chunk(now).unwrap();
    a.send_next_data_chunk(now).unwrap();
    for tsn in 1..=2 {
        a.outbound_queue.get_mut(tsn).unwrap().miss_indicator = 3;
    }
    a.will_retransmit_fast = true;
    assert!(
        a.gather_outbound_fast_retransmission_packets(vec![], now)
            .is_empty()
    );
    assert!(a.outbound_queue.is_abandoned(1));
    assert!(a.outbound_queue.is_abandoned(2));
    assert_eq!(a.outbound_queue.pending_len(), 0);
    assert_eq!(a.stream(1).unwrap().buffered_amount().unwrap(), 0);
    assert_eq!(a.my_next_tsn, 4);
    let packets = a.gather_outbound_forward_tsn_packets(vec![]);
    assert_eq!(packets.len(), 1);
    let packet = Packet::unmarshal(&packets[0]).unwrap();
    let forward = packet.chunks[0]
        .as_any()
        .downcast_ref::<ChunkForwardTsn>()
        .unwrap();
    assert_eq!(forward.new_cumulative_tsn, 3);
    assert_eq!(forward.streams[0].identifier, 1);
    assert_eq!(forward.streams[0].sequence, 0);
}

#[test]
fn test_later_fragment_exhaustion_cancels_earlier_scheduled_retry() {
    let now = Instant::now();
    let mut a = fragmented_retransmission_fixture(1);
    a.send_next_data_chunk(now).unwrap();
    a.send_next_data_chunk(now).unwrap();
    // Chunks keep independent retry counts. Once one exhausts its allowance,
    // even siblings already selected in this batch must be abandoned.
    a.outbound_queue.get_mut(2).unwrap().nsent = 2;
    a.outbound_queue.mark_all_to_retransmit();
    assert!(a.get_data_packets_to_retransmit(now).is_empty());
    assert!(a.outbound_queue.is_abandoned(1));
    assert!(a.outbound_queue.is_abandoned(2));
    assert_eq!(a.outbound_queue.pending_len(), 0);
    assert_eq!(a.stream(1).unwrap().buffered_amount().unwrap(), 0);
}

#[test]
fn test_fast_retry_counts_toward_timer_retransmission_limit() {
    for limit in [1, 2] {
        let now = Instant::now();
        let mut a = fragmented_retransmission_fixture(limit);
        for _ in 0..3 {
            a.send_next_data_chunk(now).unwrap();
        }
        for tsn in 1..=3 {
            a.outbound_queue.get_mut(tsn).unwrap().miss_indicator = 3;
        }
        a.will_retransmit_fast = true;
        assert_eq!(
            a.gather_outbound_fast_retransmission_packets(vec![], now)
                .len(),
            1
        );
        for tsn in 1..=3 {
            assert_eq!(a.outbound_queue.get(tsn).unwrap().nsent, 2);
            assert!(!a.outbound_queue.is_abandoned(tsn));
        }
        a.outbound_queue.mark_all_to_retransmit();
        let packets = a.get_data_packets_to_retransmit(now);
        assert_eq!(packets.len(), (limit - 1) as usize);
        if limit == 2 {
            a.outbound_queue.mark_all_to_retransmit();
            assert!(a.get_data_packets_to_retransmit(now).is_empty());
        }
        for tsn in 1..=3 {
            assert_eq!(a.outbound_queue.get(tsn).unwrap().nsent, limit + 1);
            assert!(a.outbound_queue.is_abandoned(tsn));
        }
    }
}

#[test]
fn abandoned_retry_message_releases_credit_without_ack_credit() {
    let now = Instant::now();
    let mut a = fragmented_retransmission_fixture(0);
    a.send_next_data_chunk(now).unwrap();
    a.send_next_data_chunk(now).unwrap();
    assert_eq!(a.stream(1).unwrap().buffered_amount().unwrap(), 10);
    a.outbound_queue.mark_all_to_retransmit();
    assert!(a.get_data_packets_to_retransmit(now).is_empty());
    assert_eq!(a.outbound_queue.inflight_bytes(), 0);
    assert_eq!(a.stream(1).unwrap().buffered_amount().unwrap(), 0);
    assert_eq!(
        a.events
            .iter()
            .filter(|event| matches!(
                event,
                Event::Stream(StreamEvent::BufferedAmountLow { id: 1 })
            ))
            .count(),
        1
    );
    let rto = a.rto_mgr.get_rto();
    let (credit, _) = a
        .process_selective_ack(
            &ChunkSelectiveAck {
                cumulative_tsn_ack: 3,
                ..Default::default()
            },
            now + Duration::from_millis(200),
        )
        .unwrap();
    assert!(credit.is_empty());
    assert_eq!(a.rto_mgr.get_rto(), rto);
    assert_eq!(a.outbound_queue.inflight_len(), 0);
}

#[test]
fn retry_abandonment_does_not_credit_reused_stream() {
    let now = Instant::now();
    let mut a = fragmented_retransmission_fixture(0);
    a.send_next_data_chunk(now).unwrap();
    // Model the old stream's removal while its retransmission metadata remains.
    a.streams.remove(&1);
    a.create_stream(1, false, PayloadProtocolIdentifier::Binary)
        .unwrap();
    a.stream(1).unwrap().write(b"new").unwrap();
    a.outbound_queue.mark_all_to_retransmit();
    assert!(a.get_data_packets_to_retransmit(now).is_empty());
    assert_eq!(a.stream(1).unwrap().buffered_amount().unwrap(), 3);
    assert_eq!(a.outbound_queue.pending_bytes(), 3);
    assert!(a.events.iter().all(|event| !matches!(
        event,
        Event::Stream(StreamEvent::BufferedAmountLow { id: 1 })
    )));
}

#[test]
fn review_fast_abandonment_cancels_pending_fragments_in_same_poll() {
    let now = Instant::now();
    let mut a = fragmented_retransmission_fixture(0);
    a.send_next_data_chunk(now).unwrap();
    // Three missing reports make the original a fast-recovery candidate.
    // The congestion window also permits the pending tail in this poll.
    a.outbound_queue.get_mut(1).unwrap().miss_indicator = 3;
    a.will_retransmit_fast = true;
    let (packets, _) = a.gather_outbound(now);
    assert!(a.outbound_queue.is_abandoned(1));
    for raw in packets {
        let packet = Packet::unmarshal(&raw).unwrap();
        for chunk in packet.chunks {
            if let Some(data) = chunk.as_any().downcast_ref::<ChunkPayloadData>() {
                assert!(
                    !a.outbound_queue.is_abandoned(data.tsn),
                    "poll emitted TSN {} after abandoning its whole message",
                    data.tsn
                );
            }
        }
    }
}

fn check_old_generation_ack_credit(gap_ack: bool) {
    use crate::chunk::chunk_selective_ack::GapAckBlock;
    let now = Instant::now();
    let mut a = fragmented_retransmission_fixture(0);
    a.send_next_data_chunk(now).unwrap();
    // Same generation transition as retry_abandonment_does_not_credit_reused_stream.
    a.streams.remove(&1);
    a.create_stream(1, false, PayloadProtocolIdentifier::Binary)
        .unwrap();
    a.stream(1).unwrap().write(b"new").unwrap();
    let sack = ChunkSelectiveAck {
        cumulative_tsn_ack: if gap_ack { 0 } else { 1 },
        gap_ack_blocks: if gap_ack {
            vec![GapAckBlock { start: 1, end: 1 }]
        } else {
            vec![]
        },
        advertised_receiver_window_credit: 100,
        ..Default::default()
    };
    a.handle_sack(&sack, now + Duration::from_millis(20))
        .unwrap();
    assert_eq!(
        a.stream(1).unwrap().buffered_amount().unwrap(),
        3,
        "old generation ACK must not release new stream credit (gap={gap_ack})"
    );
    assert!(a.events.iter().all(|event| !matches!(
        event,
        Event::Stream(StreamEvent::BufferedAmountLow { id: 1 })
    )));
}

#[test]
fn review_old_generation_cumulative_ack_preserves_new_stream_credit() {
    check_old_generation_ack_credit(false);
}

#[test]
fn review_old_generation_gap_ack_preserves_new_stream_credit() {
    check_old_generation_ack_credit(true);
}

#[test]
fn cancelled_retry_batch_reuses_window_for_unrelated_message() {
    let now = Instant::now();
    let mut a = fragmented_retransmission_fixture(1);
    for _ in 0..3 {
        a.send_next_data_chunk(now).unwrap();
    }
    a.stream(1).unwrap().write(b"live").unwrap();
    a.send_next_data_chunk(now).unwrap();
    a.outbound_queue.get_mut(2).unwrap().nsent = 2;
    a.outbound_queue.mark_all_to_retransmit();
    a.cwnd = 4;
    let packets = a.get_data_packets_to_retransmit(now);
    assert_eq!(
        packets.len(),
        1,
        "cancelled siblings must release this batch's window"
    );
    assert_eq!(packets[0].chunks.len(), 1);
    let data = packets[0].chunks[0]
        .as_any()
        .downcast_ref::<ChunkPayloadData>()
        .unwrap();
    assert_eq!(data.tsn, 4);
    assert_eq!(&data.user_data[..], b"live");
    assert_eq!(
        a.outbound_queue.get(1).unwrap().nsent,
        1,
        "cancelled selection was never sent"
    );
}

#[test]
fn gap_ack_after_abandoned_prefix_restarts_outstanding_timer() {
    let now = Instant::now();
    let mut a = fragmented_retransmission_fixture(0);
    for _ in 0..3 {
        a.send_next_data_chunk(now).unwrap();
    }
    assert!(a.abandon_if_needed(1, now));
    a.stream(1).unwrap().write(b"live").unwrap();
    a.send_next_data_chunk(now).unwrap();
    a.timers.start(Timer::T3RTX, now, 1000);
    let ack_time = now + Duration::from_millis(100);
    a.handle_sack(
        &ChunkSelectiveAck {
            cumulative_tsn_ack: 0,
            gap_ack_blocks: vec![crate::chunk::chunk_selective_ack::GapAckBlock {
                start: 4,
                end: 4,
            }],
            advertised_receiver_window_credit: 100,
            ..Default::default()
        },
        ack_time,
    )
    .unwrap();
    assert_eq!(
        a.timers.get(Timer::T3RTX),
        Some(ack_time + Duration::from_millis(a.rto_mgr.get_rto()))
    );
}

#[test]
fn late_ack_of_abandoned_data_resets_errors_but_duplicates_do_not() {
    let now = Instant::now();
    let mut a = fragmented_retransmission_fixture(0);
    a.send_next_data_chunk(now).unwrap();
    assert!(a.abandon_if_needed(1, now));
    a.timers = TimerTable::new(None, Some(1), 60000);
    a.timers.start(Timer::T3RTX, now, 1);
    assert_eq!(
        a.timers
            .is_expired(Timer::T3RTX, now + Duration::from_millis(1)),
        (true, false, 1)
    );
    let sack = ChunkSelectiveAck {
        cumulative_tsn_ack: 0,
        gap_ack_blocks: vec![crate::chunk::chunk_selective_ack::GapAckBlock { start: 1, end: 1 }],
        advertised_receiver_window_credit: 100,
        ..Default::default()
    };
    a.handle_sack(&sack, now + Duration::from_millis(2))
        .unwrap();
    a.timers.start(Timer::T3RTX, now, 1);
    assert_eq!(
        a.timers
            .is_expired(Timer::T3RTX, now + Duration::from_millis(2)),
        (true, false, 1)
    );
    a.handle_sack(&sack, now + Duration::from_millis(3))
        .unwrap();
    a.timers.start(Timer::T3RTX, now, 1);
    assert_eq!(
        a.timers
            .is_expired(Timer::T3RTX, now + Duration::from_millis(4)),
        (true, true, 2)
    );
}

#[test]
fn forward_only_marker_ack_does_not_reset_peer_errors() {
    let now = Instant::now();
    let mut a = fragmented_retransmission_fixture(0);
    a.send_next_data_chunk(now).unwrap();
    assert!(a.abandon_if_needed(1, now));
    assert_eq!(a.outbound_queue.get(2).unwrap().nsent, 0);
    a.timers = TimerTable::new(None, Some(1), 60000);
    a.timers.start(Timer::T3RTX, now, 1);
    assert_eq!(
        a.timers
            .is_expired(Timer::T3RTX, now + Duration::from_millis(1)),
        (true, false, 1)
    );
    a.handle_sack(
        &ChunkSelectiveAck {
            cumulative_tsn_ack: 0,
            gap_ack_blocks: vec![crate::chunk::chunk_selective_ack::GapAckBlock {
                start: 2,
                end: 2,
            }],
            advertised_receiver_window_credit: 100,
            ..Default::default()
        },
        now + Duration::from_millis(2),
    )
    .unwrap();
    a.timers.start(Timer::T3RTX, now, 1);
    assert_eq!(
        a.timers
            .is_expired(Timer::T3RTX, now + Duration::from_millis(4)),
        (true, true, 2)
    );
}
