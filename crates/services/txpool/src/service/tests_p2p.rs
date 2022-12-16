use super::*;
use crate::service::test_helpers::TestContext;
use fuel_core_interfaces::txpool::TxPoolMpsc;
use fuel_core_types::fuel_tx::{
    Transaction,
    UniqueIdentifier,
};
use std::ops::Deref;
use tokio::sync::{
    mpsc::error::TryRecvError,
    oneshot,
};

#[tokio::test]
async fn can_insert_from_p2p() {
    let ctx = TestContext::new().await;
    let service = ctx.service();
    let tx1 = ctx.setup_script_tx(10);

    let broadcast_tx = TransactionGossipData::new(
        TransactionBroadcast::NewTransaction(tx1.clone()),
        vec![],
        vec![],
    );
    let mut receiver = service.tx_update_subscribe();
    let res = ctx.gossip_tx.send(broadcast_tx).unwrap();
    let _ = receiver.recv().await;
    assert_eq!(1, res);

    let (response, receiver) = oneshot::channel();
    let _ = service
        .sender()
        .send(TxPoolMpsc::Find {
            ids: vec![tx1.id()],
            response,
        })
        .await;
    let out = receiver.await.unwrap();

    let got_tx: Transaction = out[0].as_ref().unwrap().tx().clone().deref().into();
    assert_eq!(tx1, got_tx);
}

#[tokio::test]
async fn insert_from_local_broadcasts_to_p2p() {
    let ctx = TestContext::new().await;
    let tx1 = Arc::new(ctx.setup_script_tx(10));
    let service = ctx.service();
    let mut subscribe_status = service.tx_status_subscribe();
    let mut subscribe_update = service.tx_update_subscribe();

    let (response, receiver) = oneshot::channel();
    let _ = service
        .sender()
        .send(TxPoolMpsc::Insert {
            txs: vec![tx1.clone()],
            response,
        })
        .await;
    let out = receiver.await.unwrap();

    if let Ok(result) = &out[0] {
        // we are sure that included tx are already broadcasted.
        assert_eq!(
            subscribe_status.try_recv(),
            Ok(TxStatus::Submitted),
            "First added should be tx1"
        );
        let update = subscribe_update.try_recv().unwrap();
        assert_eq!(
            *update.tx_id(),
            result.inserted.id(),
            "First added should be tx1"
        );
    } else {
        panic!("Tx1 should be OK, got err");
    }

    let ret = ctx.p2p_request_rx.lock().await.recv().await.unwrap();

    if let P2pRequestEvent::BroadcastNewTransaction { transaction } = ret {
        assert_eq!(tx1, transaction);
    } else {
        panic!("Transaction Broadcast Unwrap Failed");
    }
}

#[tokio::test]
async fn test_insert_from_p2p_does_not_broadcast_to_p2p() {
    let ctx = TestContext::new().await;
    let service = ctx.service();

    let tx1 = ctx.setup_script_tx(10);
    let broadcast_tx = TransactionGossipData::new(
        TransactionBroadcast::NewTransaction(tx1.clone()),
        vec![],
        vec![],
    );
    let mut receiver = service.tx_update_subscribe();
    let res = ctx.gossip_tx.send(broadcast_tx).unwrap();
    let _ = receiver.recv().await;
    assert_eq!(1, res);

    let ret = ctx.p2p_request_rx.lock().await.try_recv();
    assert!(ret.is_err());
    assert_eq!(Some(TryRecvError::Empty), ret.err());
}
