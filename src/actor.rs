//! 轻量 actor 框架
//!
//! v0.34: 为把领域状态（event/schedule/ccr/mock/trace）从 `Arc<Mutex<State>>`
//! 拆成 actor/通道而引入。基于 tokio::sync::mpsc + oneshot 实现请求/响应。

use std::future::Future;
use std::pin::Pin;
use tokio::sync::{mpsc, oneshot};

/// Actor 对外句柄。`M` 为该 actor 接受的消息枚举。
#[derive(Clone, Debug)]
pub struct ActorHandle<M> {
    tx: mpsc::UnboundedSender<M>,
}

impl<M> ActorHandle<M> {
    /// 向 actor 发送消息（fire-and-forget）。
    pub fn tell(&self, msg: M) {
        // 若 actor 已停止，消息静默丢弃；这是 actor 模型的常见语义。
        let _ = self.tx.send(msg);
    }

    /// 向 actor 发送请求并等待响应。
    /// 消息构造器 `f` 接收 oneshot sender，返回完整消息。
    pub async fn ask<R>(&self, f: impl FnOnce(oneshot::Sender<R>) -> M) -> Result<R, ActorError> {
        let (tx, rx) = oneshot::channel();
        self.tx.send(f(tx)).map_err(|_| ActorError::ActorStopped)?;
        rx.await.map_err(|_| ActorError::ActorStopped)
    }
}

impl<M> PartialEq for ActorHandle<M> {
    fn eq(&self, _other: &Self) -> bool {
        // 句柄只按存在性比较，不比较内部通道 identity。
        true
    }
}

impl<M> Eq for ActorHandle<M> {}

/// Actor 错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActorError {
    /// Actor 已停止或通道已关闭。
    ActorStopped,
    /// 业务层返回的字符串错误。
    Business(String),
}

impl std::fmt::Display for ActorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ActorError::ActorStopped => write!(f, "actor stopped"),
            ActorError::Business(s) => write!(f, "{}", s),
        }
    }
}

impl std::error::Error for ActorError {}

impl From<String> for ActorError {
    fn from(s: String) -> Self {
        ActorError::Business(s)
    }
}

impl From<&str> for ActorError {
    fn from(s: &str) -> Self {
        ActorError::Business(s.to_string())
    }
}

/// Handler future 类型：可借用 `&mut S` 的异步块。
pub type ActorFuture<'a> = Pin<Box<dyn Future<Output = ()> + Send + 'a>>;

/// 启动一个 actor。
///
/// `handler` 接收可变状态 `S` 和消息 `M`，返回 future；在 tokio task 中无限循环处理消息。
/// 当所有 `ActorHandle` 被 drop 时，通道关闭，actor 自动退出。
pub fn spawn_actor<S, M, F>(mut state: S, mut handler: F) -> ActorHandle<M>
where
    S: Send + 'static,
    M: Send + 'static,
    F: for<'a> FnMut(&'a mut S, M) -> ActorFuture<'a> + Send + 'static,
{
    let (tx, mut rx) = mpsc::unbounded_channel::<M>();
    tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            handler(&mut state, msg).await;
        }
    });
    ActorHandle { tx }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Counter {
        value: i64,
    }

    enum CounterMsg {
        Inc(oneshot::Sender<i64>),
        Get(oneshot::Sender<i64>),
    }

    #[tokio::test]
    async fn ask_and_tell_work() {
        let handle = spawn_actor(Counter::default(), |state, msg| {
            Box::pin(async move {
                match msg {
                    CounterMsg::Inc(reply) => {
                        state.value += 1;
                        let _ = reply.send(state.value);
                    }
                    CounterMsg::Get(reply) => {
                        let _ = reply.send(state.value);
                    }
                }
            })
        });

        let v1 = handle.ask(CounterMsg::Inc).await.unwrap();
        assert_eq!(v1, 1);
        let v2 = handle.ask(CounterMsg::Inc).await.unwrap();
        assert_eq!(v2, 2);
        let v3 = handle.ask(CounterMsg::Get).await.unwrap();
        assert_eq!(v3, 2);
    }

    #[tokio::test]
    async fn actor_stops_when_handle_dropped() {
        let handle = spawn_actor(Counter::default(), |_state, _msg: CounterMsg| {
            Box::pin(async move {})
        });
        drop(handle);
        // actor 已经退出，但这里不等待；主要验证 drop 不会 panic。
    }
}
