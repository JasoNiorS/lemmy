use crate::{
  community::BanFromCommunity,
  context::LemmyContext,
  person::BanPerson,
  post::{DeletePost, RemovePost},
};
use activitypub_federation::config::Data;
use futures::future::BoxFuture;
use lemmy_db_schema::{
  newtypes::{CommunityId, DbUrl, PersonId},
  source::{
    comment::Comment,
    community::Community,
    person::Person,
    post::Post,
    private_message::PrivateMessage,
  },
};
use lemmy_db_views::structs::PrivateMessageView;
use lemmy_utils::{error::LemmyResult, SYNCHRONOUS_FEDERATION};
use once_cell::sync::{Lazy, OnceCell};
use tokio::{
  sync::{
    mpsc,
    mpsc::{UnboundedReceiver, UnboundedSender, WeakUnboundedSender},
    Mutex,
  },
  task::JoinHandle,
};
use url::Url;

type MatchOutgoingActivitiesBoxed =
  Box<for<'a> fn(SendActivityData, &'a Data<LemmyContext>) -> BoxFuture<'a, LemmyResult<()>>>;

/// This static is necessary so that activities can be sent out synchronously for tests.
pub static MATCH_OUTGOING_ACTIVITIES: OnceCell<MatchOutgoingActivitiesBoxed> = OnceCell::new();

#[derive(Debug)]
pub enum SendActivityData {
  CreatePost(Post),
  UpdatePost(Post),
  DeletePost(Post, Person, DeletePost),
  RemovePost(Post, Person, RemovePost),
  LockPost(Post, Person, bool),
  FeaturePost(Post, Person, bool),
  CreateComment(Comment),
  UpdateComment(Comment),
  DeleteComment(Comment, Person, Community),
  RemoveComment(Comment, Person, Community, Option<String>),
  LikePostOrComment(DbUrl, Person, Community, i16),
  FollowCommunity(Community, Person, bool),
  UpdateCommunity(Person, Community),
  DeleteCommunity(Person, Community, bool),
  RemoveCommunity(Person, Community, Option<String>, bool),
  AddModToCommunity(Person, CommunityId, PersonId, bool),
  BanFromCommunity(Person, CommunityId, Person, BanFromCommunity),
  BanFromSite(Person, Person, BanPerson),
  CreatePrivateMessage(PrivateMessageView),
  UpdatePrivateMessage(PrivateMessageView),
  DeletePrivateMessage(Person, PrivateMessage, bool),
  DeleteUser(Person),
  CreateReport(Url, Person, Community, String),
}

// TODO: instead of static, move this into LemmyContext. make sure that stopping the process with
//       ctrl+c still works.
static ACTIVITY_CHANNEL: Lazy<ActivityChannel> = Lazy::new(|| {
  let (sender, receiver) = mpsc::unbounded_channel();
  let weak_sender = sender.downgrade();
  ActivityChannel {
    weak_sender,
    receiver: Mutex::new(receiver),
    keepalive_sender: Mutex::new(Some(sender)),
  }
});

pub struct ActivityChannel {
  weak_sender: WeakUnboundedSender<SendActivityData>,
  receiver: Mutex<UnboundedReceiver<SendActivityData>>,
  keepalive_sender: Mutex<Option<UnboundedSender<SendActivityData>>>,
}

impl ActivityChannel {
  pub async fn retrieve_activity() -> Option<SendActivityData> {
    let mut lock = ACTIVITY_CHANNEL.receiver.lock().await;
    lock.recv().await
  }

  pub async fn submit_activity(
    data: SendActivityData,
    context: &Data<LemmyContext>,
  ) -> LemmyResult<()> {
    if *SYNCHRONOUS_FEDERATION {
      MATCH_OUTGOING_ACTIVITIES
        .get()
        .expect("retrieve function pointer")(data, context)
      .await?;
    }
    // could do `ACTIVITY_CHANNEL.keepalive_sender.lock()` instead and get rid of weak_sender,
    // not sure which way is more efficient
    else if let Some(sender) = ACTIVITY_CHANNEL.weak_sender.upgrade() {
      sender.send(data)?;
    }
    Ok(())
  }

  pub async fn close(outgoing_activities_task: JoinHandle<LemmyResult<()>>) -> LemmyResult<()> {
    ACTIVITY_CHANNEL.keepalive_sender.lock().await.take();
    outgoing_activities_task.await??;
    Ok(())
  }
}
