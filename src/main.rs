use rand::Rng;
use regex::Regex;
use reqwest::get;
use serenity::client::Client;
use serenity::client::Context;
use serenity::client::EventHandler;
use serenity::Result as SerenityResult;
use serenity::async_trait;
use serenity::http::Http;
use serenity::model::channel::Message;
use serenity::model::gateway::Ready;
use serenity::model::prelude::ChannelId;
use serenity::model::prelude::GuildId;
use serenity::prelude::GatewayIntents;
use songbird::Call;
use songbird::Config;
use songbird::Event;
use songbird::EventContext;
use songbird::EventHandler as VoiceEventHandler;
use songbird::SerenityInit;
use songbird::TrackEvent;
use songbird::driver::DecodeMode;
use songbird::error::JoinError;
use songbird::input::restartable::Restartable;
use songbird::tracks::TrackHandle;
use std::env;
use std::ops::Add;
use std::sync::Arc;
use std::time::Duration;
use std::time::SystemTime;
use tokio::sync::Mutex;
use url::Url;
use url_escape::encode_fragment;

const PREFIX: &str = "!";
const EMPTY: &str = "";
const IDLE_TIME: u64 = 60;

enum CommandError {
    InvalidCommand(String),
    MissingUrl(String),
    MissingSource,
    MissingSongbird,
    JoinError(JoinError),
}

struct CommonHandler;

impl CommonHandler {
    async fn play(ctx: &Context, guild_id: GuildId, voice_channel_id: ChannelId, msg_channel_id: ChannelId, args: &str) -> Result<(Url, TrackHandle), CommandError> {
        let Some(url) = CommonHandler::solve_url(args).await else {
            return Err(CommandError::MissingUrl(args.to_string()));
        };

        let handler = match CommonHandler::get_songbird_handler(ctx, guild_id, voice_channel_id, msg_channel_id).await {
            Ok(handler) => {
                handler
            }
            Err(err) => {
                return Err(err);
            }
        };

        let Some(source) = CommonHandler::get_source(url.clone()).await else {
            return Err(CommandError::MissingSource);
        };

        let track_handle = {
            let mut handler = handler.lock().await;

            handler.enqueue_source(source.into())
        };

        Ok((url, track_handle))
    }

    async fn random(ctx: &Context, guild_id: GuildId, voice_channel_id: ChannelId, msg_channel_id: ChannelId, args: &str) -> Result<(Url, TrackHandle), CommandError> {
        let Some(url) = CommonHandler::random_url(args).await else {
            return Err(CommandError::MissingUrl(args.to_string()));
        };

        let handler = match CommonHandler::get_songbird_handler(ctx, guild_id, voice_channel_id, msg_channel_id).await {
            Ok(handler) => {
                handler
            }
            Err(err) => {
                return Err(err);
            }
        };

        let Some(source) = CommonHandler::get_source(url.clone()).await else {
            return Err(CommandError::MissingSource);
        };

        let track_handle = {
            let mut handler = handler.lock().await;

            handler.enqueue_source(source.into())
        };

        Ok((url, track_handle))
    }

    async fn skip(ctx: &Context, guild_id: GuildId, voice_channel_id: ChannelId, msg_channel_id: ChannelId, _args: &str) -> Result<(), CommandError> {
        let handler = match CommonHandler::get_songbird_handler(ctx, guild_id, voice_channel_id, msg_channel_id).await {
            Ok(handler) => {
                handler
            }
            Err(err) => {
                return Err(err);
            }
        };

        {
            let handler = handler.lock().await;

            let queue = handler.queue();

            let _ = queue.skip();
        }

        Ok(())
    }

    async fn stop(ctx: &Context, guild_id: GuildId, voice_channel_id: ChannelId, msg_channel_id: ChannelId, _args: &str) -> Result<(), CommandError> {
        let handler = match CommonHandler::get_songbird_handler(ctx, guild_id, voice_channel_id, msg_channel_id).await {
            Ok(handler) => {
                handler
            }
            Err(err) => {
                return Err(err);
            }
        };

        {
            let mut handler = handler.lock().await;

            handler.stop();
        }

        Ok(())
    }

    async fn solve_url(args: &str) -> Option<Url> {
        if let Ok(url) = Url::parse(args) {
            return Some(url);
        }

        CommonHandler::search(args, 0).await
    }

    async fn random_url(args: &str) -> Option<Url> {
        if let Ok(url) = Url::parse(args) {
            return Some(url);
        }
        
        let idx = {
            let mut rng = rand::thread_rng();
            rng.gen::<usize>() % 12
        };

        CommonHandler::search(args, idx).await
    }

    async fn get_songbird_handler(ctx: &Context, guild_id: GuildId, voice_channel_id: ChannelId, msg_channel_id: ChannelId) -> Result<Arc<Mutex<Call>>, CommandError> {
        let Some(manager) = songbird::get(ctx).await else {
            return Err(CommandError::MissingSongbird);
        };

        match manager.get(guild_id) {
            Some(handler) => {
                Ok(handler)
            }
            None => {
                let (handler, join_result) = manager
                    .join(guild_id, voice_channel_id)
                    .await;

                match join_result {
                    Ok(()) => {
                        let idle_notifier = IdleNotifier::new(
                            msg_channel_id,
                            ctx.http.clone(),
                            handler.clone()
                        );

                        {
                            let mut handler = handler
                                .lock()
                                .await;

                            handler.add_global_event(
                                Event::Periodic(Duration::from_secs(IDLE_TIME), None),
                                idle_notifier
                            );
                        }

                        Ok(handler)
                    }
                    Err(err) => {
                        Err(CommandError::JoinError(err))
                    }
                }
            }
        }
    }

    async fn get_source(url: Url) -> Option<Restartable> {
        Restartable::ytdl(url, true).await.ok()
    }

    async fn search(query: &str, idx: usize) -> Option<Url> {
        let re = Regex::new(r"watch\?v=...........").unwrap();
        let query = encode_fragment(&query);
        let search_url = format!("https://www.youtube.com/results?search_query={}", query);

        let Ok(resp) = get(search_url).await else {
            return None;
        };

        let Ok(text) = resp.text().await else {
            return None;
        };

        let mut caps = re.captures_iter(&text);

        for _ in 0..idx {
            caps.next();
        }

        let Some(first) = caps.next() else {
            return None;
        };

        let Some(first) = first.get(0) else {
            return None;
        };

        let first = format!("https://www.youtube.com/{}", first.as_str());

        let Ok(url) = Url::parse(&first) else {
            return None;
        };

        Some(url)
    }
}

#[async_trait]
impl EventHandler for CommonHandler {
    async fn ready(&self, _ctx: Context, ready: Ready) {
        println!("{} is connected!", ready.user.name);
    }

    async fn message(&self, ctx: Context, msg: Message) {

        // Remove own messages
        if msg.is_own(&ctx.cache) {
            return;
        }

        // Remove common messages
        if !msg.content.starts_with(PREFIX) {
            return;
        }

        // Split command and arguments
        let (cmd, args) = match msg.content.split_once(' ') {
            Some(pair) => pair,
            None       => (msg.content.as_str(), EMPTY),
        };

        let cmd = &cmd[1..];

        // Get ids
        let Some(guild) = msg.guild(&ctx.cache) else {
            check_msg(
                msg
                    .channel_id
                    .say(&ctx.http, "Ta achando que eu vou narrar a musica? Entra em um canal de voz ai plmds".to_string())
                    .await
            );

            return;
        };

        let guild_id = guild.id;

        let Some(voice_channel_id) = guild
            .voice_states
            .get(&msg.author.id)
            .and_then(|voice_state| voice_state.channel_id) else {
                check_msg(
                    msg
                        .channel_id
                        .say(&ctx.http, "Ta achando que eu vou narrar a musica? Entra em um canal de voz ai plmds".to_string())
                        .await
                );

                return;
            };

        // Identify command
        let result = match cmd {
            "play" => {
                match CommonHandler::play(&ctx, guild_id, voice_channel_id, msg.channel_id, args).await {
                    Ok((url, track_handle)) => {
                        let _ = track_handle.add_event(
                            Event::Track(TrackEvent::Play),
                            TrackStartNotifier::new(
                                voice_channel_id,
                                ctx.http.clone(),
                                url.clone(),
                            ),
                        );

                        check_msg(
                            msg
                                .channel_id
                                .say(&ctx.http, &format!("Botei na fila: {}", url))
                                .await
                        );

                        Ok(())
                    }
                    Err(err) => {
                        Err(err)
                    }
                }
            }
            "random" => {
                match CommonHandler::random(&ctx, guild_id, voice_channel_id, msg.channel_id, args).await {
                    Ok((url, track_handle)) => {
                        let _ = track_handle.add_event(
                            Event::Track(TrackEvent::Play),
                            TrackStartNotifier::new(
                                voice_channel_id,
                                ctx.http.clone(),
                                url.clone(),
                            ),
                        );

                        check_msg(
                            msg
                                .channel_id
                                .say(&ctx.http, &format!("Botei na fila: {}", url))
                                .await
                        );

                        Ok(())
                    }
                    Err(err) => {
                        Err(err)
                    }
                }
            }
            "skip" => {
                CommonHandler::skip(&ctx, guild_id, voice_channel_id, msg.channel_id, args).await
            }
            "stop" => {
                CommonHandler::stop(&ctx, guild_id, voice_channel_id, msg.channel_id, args).await
            }
            cmd => {
                Err(CommandError::InvalidCommand(cmd.to_string()))
            }
        };

        if let Err(err) = result {
            let response = match err {
                CommandError::InvalidCommand(cmd) => {
                    format!("Sim, {} conheço ele, meu amigo", cmd)
                }
                CommandError::MissingUrl(query) => {
                    format!("\"{}\" eu não entendi, escreve igual gente agora", query)
                }
                CommandError::MissingSource => {
                    format!("Deu um biricutico aqui e não deu pra baixar a música")
                }
                CommandError::MissingSongbird => {
                    format!("Deu um biricutico aqui, reclame com o DEV")
                }
                CommandError::JoinError(join_error) => {
                    println!("JoinError {:?}", join_error);
                    format!("Estão robando meus dados! Não consegui entrar na sala")
                }
            };

            check_msg(
                msg
                    .channel_id
                    .say(&ctx.http, &response)
                    .await
            );
        }
    }
}

struct TrackStartNotifier {
    msg_channel_id: ChannelId,
    http:           Arc<Http>,
    url:            Url,
}

impl TrackStartNotifier {
    fn new(msg_channel_id: ChannelId, http: Arc<Http>, url: Url) -> Self {
        Self {
            msg_channel_id,
            http,
            url,
        }
    }
}

#[async_trait]
impl VoiceEventHandler for TrackStartNotifier {
    async fn act(&self, ctx: &EventContext<'_>) -> Option<Event> {
        if let EventContext::Track(_track_list) = ctx {
            check_msg(
                self.msg_channel_id
                    .say(&self.http, format!("Tocando {}", self.url))
                    .await
            );
        }

        None
    }
}

#[derive(Clone)]
struct IdleNotifier {
    msg_channel_id: ChannelId,
    http:           Arc<Http>,
    handler:        Arc<Mutex<Call>>,
    last_action:    Arc<Mutex<SystemTime>>,
}

impl IdleNotifier {
    fn new(msg_channel_id: ChannelId, http: Arc<Http>, handler: Arc<Mutex<Call>>) -> Self {
        Self {
            msg_channel_id,
            http,
            handler,
            last_action: Arc::new(Mutex::new(SystemTime::now())),
        }
    }
}

#[async_trait]
impl VoiceEventHandler for IdleNotifier {
    async fn act(&self, ctx: &EventContext<'_>) -> Option<Event> {
        if let EventContext::Track(track_list) = ctx {
            let now = SystemTime::now();
            let mut last_action = self.last_action
                .lock()
                .await;

            if track_list.len() > 0 {
                *last_action = now;
            } else if last_action.add(Duration::new(IDLE_TIME, 0)) < now {
                check_msg(
                    self.msg_channel_id
                        .say(&self.http, "Vlw, flw")
                        .await
                );

                let mut handler = self.handler
                    .lock()
                    .await;
                let _ = handler.leave().await;

                return Some(Event::Cancel);
            }
        }

        None
    }
}

fn check_msg(result: SerenityResult<Message>) {
    if let Err(why) = result {
        println!("Error sending message: {:?}", why);
    }
}

#[tokio::main]
async fn main() {
    let token = env::var("DISCORD_TOKEN")
        .expect("DISCORD_TOKEN env var is missing");

    let intents = GatewayIntents::non_privileged()
        | GatewayIntents::MESSAGE_CONTENT
        | GatewayIntents::GUILD_VOICE_STATES;

    let songbird_config = Config::default()
        .decode_mode(DecodeMode::Decode);

    let mut client = Client::builder(&token, intents)
        .event_handler(CommonHandler)
        .register_songbird_from_config(songbird_config)
        .await
        .expect("Error creating client");

    tokio::spawn(
        async move {
            let _ = client
                .start()
                .await
                .map_err(|why| println!("Client ended: {:?}", why));
        }
    );

    let _ = tokio::signal::ctrl_c().await;

    println!("Received Ctrl-C, shutting down.");
}