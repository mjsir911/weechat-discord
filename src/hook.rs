use crate::utils::ChannelExt;
use crate::{discord, on_main, plugin_print, utils};
use crossbeam_channel::unbounded;
use serenity::{model::prelude::*, prelude::*};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use weechat::{Buffer, CompletionPosition, ConfigOption, ReturnCode, Weechat};

static mut LAST_TYPING_TIMESTAMP: u64 = 0;

pub struct HookHandles {
    _buffer_switch_handle: weechat::SignalHook<()>,
    _buffer_typing_handle: weechat::SignalHook<()>,
    _command_handle: weechat::CommandHook<()>,
    _query_handle: weechat::CommandRunHook<()>,
    _nick_handle: weechat::CommandRunHook<()>,
    _join_handle: weechat::CommandRunHook<()>,
    _guild_completion_handle: weechat::CompletionHook<()>,
    _channel_completion_handle: weechat::CompletionHook<()>,
    _dm_completion_handle: weechat::CompletionHook<()>,
    _nick_completion_handle: weechat::CompletionHook<()>,
    _role_completion_handle: weechat::CompletionHook<()>,
}

pub fn init(weechat: &Weechat) -> HookHandles {
    let _command_handle = crate::command::init(weechat);

    let _buffer_switch_handle = weechat.hook_signal(
        "buffer_switch",
        |_, _, value| handle_buffer_switch(value),
        None,
    );

    let _buffer_typing_handle = weechat.hook_signal(
        "input_text_changed",
        |_, weechat, value| handle_buffer_typing(weechat, value),
        None,
    );

    let _query_handle = weechat.hook_command_run(
        "/query",
        |_, ref buffer, ref command| {
            if buffer.get_localvar("guildid").is_none() {
                return ReturnCode::Error;
            };

            handle_query(command)
        },
        None,
    );

    let _nick_handle = weechat.hook_command_run(
        "/nick",
        |_, ref buffer, ref command| handle_nick(buffer, command),
        None,
    );

    let _join_handle = weechat.hook_command_run(
        "/join",
        |_, ref buffer, ref command| handle_join(buffer, command),
        None,
    );

    let _guild_completion_handle = weechat.hook_completion(
        "weecord_guild_completion",
        "Completion for discord guilds",
        |_, ref buffer, _, completions| handle_guild_completion(buffer, completions),
        None,
    );

    let _channel_completion_handle = weechat.hook_completion(
        "weecord_channel_completion",
        "Completion for discord channels",
        |_, ref buffer, _, completions| handle_channel_completion(buffer, completions),
        None,
    );

    let _dm_completion_handle = weechat.hook_completion(
        "weecord_dm_completion",
        "Completion for Discord private channels",
        |_, ref buffer, _, completions| handle_dm_completion(buffer, completions),
        None,
    );

    let _nick_completion_handle = weechat.hook_completion(
        "nicks",
        "Completion for users in a buffer",
        |_, ref buffer, _, completions| handle_nick_completion(buffer, completions),
        None,
    );

    let _role_completion_handle = weechat.hook_completion(
        "weecord_role",
        "Completion for Discord channel roles",
        |_, ref buffer, _, completions| handle_role_completion(buffer, completions),
        None,
    );

    HookHandles {
        _buffer_switch_handle,
        _buffer_typing_handle,
        _command_handle,
        _query_handle,
        _nick_handle,
        _join_handle,
        _guild_completion_handle,
        _channel_completion_handle,
        _dm_completion_handle,
        _nick_completion_handle,
        _role_completion_handle,
    }
}

pub fn buffer_input(buffer: Buffer, text: &str) {
    let channel = buffer
        .get_localvar("channelid")
        .and_then(|id| id.parse().ok())
        .map(ChannelId);

    let guild = buffer
        .get_localvar("guildid")
        .and_then(|id| id.parse().ok())
        .map(GuildId);

    if let Some(channel) = channel {
        let ctx = match crate::discord::get_ctx() {
            Some(ctx) => ctx,
            _ => return,
        };

        if let Some(edit) = parsing::parse_line_edit(text) {
            let weechat = buffer.get_weechat();
            match edit {
                parsing::LineEdit::Delete { line } => {
                    if let Err(e) = crate::utils::get_users_nth_message(&ctx, channel, line)
                        .map(|msg| channel.delete_message(&ctx.http, msg.id))
                    {
                        buffer.print(&format!(
                            "[discord] An error occurred deleting a message: {}",
                            e
                        ));
                    } else {
                        buffer.print(&format!(
                            "{}\tMessage ({}) deleted",
                            weechat.get_prefix("network"),
                            line,
                        ))
                    }
                }
                parsing::LineEdit::Sub {
                    line,
                    old,
                    new,
                    options,
                } => {
                    // TODO: Clean this up, (try block)?
                    if let Err(e) =
                        crate::utils::get_users_nth_message(&ctx, channel, line).map(|mut msg| {
                            let orig = msg.content.clone();
                            msg.edit(ctx, |e| {
                                if options.map(|o| o.contains('g')).unwrap_or_default() {
                                    e.content(orig.replace(old, new))
                                } else {
                                    e.content(orig.replacen(old, new, 1))
                                }
                            })
                        })
                    {
                        buffer.print(&format!(
                            "[discord] An error occurred editing a message: {}",
                            e
                        ));
                    } else {
                        buffer.print(&format!(
                            "{}\t{}s/{}/{}/{}",
                            weechat.get_prefix("network"),
                            line,
                            old,
                            new,
                            options.unwrap_or_default()
                        ))
                    }
                }
            }
            return;
        }
        let text = utils::create_mentions(&ctx.cache, guild, text);
        channel
            .say(ctx, text)
            .unwrap_or_else(|_| panic!("Unable to send message to {}", channel.0));
    }
}

fn handle_buffer_switch(data: weechat::SignalHookValue) -> ReturnCode {
    if let weechat::SignalHookValue::Pointer(buffer_ptr) = data {
        let buffer = unsafe { crate::utils::buffer_from_ptr(buffer_ptr) };

        // Wait until messages have been loaded to acknowledge them
        let (tx, rx) = unbounded();
        if buffer.get_localvar("loaded_history").is_none() {
            crate::buffers::load_history(&buffer, tx);
        }

        if buffer.get_localvar("loaded_nicks").is_none() {
            crate::buffers::load_nicks(&buffer);
        }

        let channel_id = buffer
            .get_localvar("channelid")
            .and_then(|id| id.parse().ok())
            .map(ChannelId);

        thread::spawn(move || {
            if let Err(_) = rx.recv() {
                return;
            }
            let ctx = match discord::get_ctx() {
                Some(s) => s,
                None => return,
            };
            if let Some(channel) = channel_id.and_then(|id| id.to_channel_cached(&ctx)) {
                if let Some(rs) = ctx.cache.read().read_state.get(&channel.id()) {
                    if let Some(last_message_id) = channel.last_message() {
                        if rs.last_message_id != last_message_id {
                            let _ = channel.id().ack_message(&ctx, last_message_id);
                        }
                    }
                }
            }
        });
    }
    ReturnCode::Ok
}

fn handle_buffer_typing(weechat: &Weechat, data: weechat::SignalHookValue) -> ReturnCode {
    if let weechat::SignalHookValue::Pointer(buffer_ptr) = data {
        let buffer = unsafe { crate::utils::buffer_from_ptr(buffer_ptr) };
        if let Some(chnanel_id) = buffer.get_localvar("channelid") {
            if crate::upgrade_plugin(weechat)
                .config
                .send_typing_events
                .value()
            {
                if buffer.input().starts_with('/') {
                    return ReturnCode::Ok;
                }
                if let Ok(channel_id) = chnanel_id.as_ref().parse().map(ChannelId) {
                    // TODO: Wait for user to type for 3 seconds
                    let now = SystemTime::now();
                    let timestamp_now = now
                        .duration_since(UNIX_EPOCH)
                        .expect("Time went backwards")
                        .as_secs() as u64;

                    if unsafe { LAST_TYPING_TIMESTAMP } + 9 < timestamp_now {
                        unsafe { LAST_TYPING_TIMESTAMP = timestamp_now }
                        std::thread::spawn(move || {
                            let ctx = match discord::get_ctx() {
                                Some(s) => s,
                                None => return,
                            };
                            let _ = channel_id.broadcast_typing(&ctx.http);
                        });
                    }
                }
            }
        }
    }
    ReturnCode::Ok
}

fn handle_channel_completion(buffer: &Buffer, completion: weechat::Completion) -> ReturnCode {
    // Get the previous argument with should be the guild name
    // TODO: Generalize this?
    let input = buffer.input();
    let x = input.split(' ').collect::<Vec<_>>();
    let input = if x.len() < 2 {
        None
    } else {
        Some(x[x.len() - 2].to_owned())
    };

    let input = match input {
        Some(i) => i,
        None => return ReturnCode::Ok,
    };

    // Match mangled name to the real name
    let ctx = match discord::get_ctx() {
        Some(s) => s,
        None => return ReturnCode::Ok,
    };

    for guild in ctx.cache.read().guilds.values() {
        let guild = guild.read();
        if parsing::weechat_arg_strip(&guild.name).to_lowercase() == input.to_lowercase() {
            for channel in guild.channels.values() {
                let channel = channel.read();
                // Skip non text channels
                use serenity::model::channel::ChannelType::*;
                match channel.kind {
                    Text | Private | Group | News => {}
                    _ => continue,
                }
                let permissions = guild.user_permissions_in(channel.id, ctx.cache.read().user.id);
                if !permissions.read_message_history() || !permissions.read_messages() {
                    continue;
                }
                completion.add(&parsing::weechat_arg_strip(&channel.name))
            }
            return ReturnCode::Ok;
        }
    }
    ReturnCode::Ok
}

fn handle_guild_completion(_buffer: &Buffer, completion: weechat::Completion) -> ReturnCode {
    let ctx = match discord::get_ctx() {
        Some(s) => s,
        None => return ReturnCode::Ok,
    };
    for guild in ctx.cache.read().guilds.values() {
        let name = parsing::weechat_arg_strip(&guild.read().name);
        completion.add(&name);
    }
    ReturnCode::Ok
}

fn handle_dm_completion(_buffer: &Buffer, completion: weechat::Completion) -> ReturnCode {
    let ctx = match discord::get_ctx() {
        Some(s) => s,
        None => return ReturnCode::Ok,
    };
    for dm in ctx.cache.read().private_channels.values() {
        completion.add(&dm.read().recipient.read().name);
    }
    ReturnCode::Ok
}

fn handle_nick_completion(buffer: &Buffer, completion: weechat::Completion) -> ReturnCode {
    let ctx = match discord::get_ctx() {
        Some(s) => s,
        None => return ReturnCode::Ok,
    };

    let channel_id = buffer
        .get_localvar("channelid")
        .and_then(|id| id.parse().ok())
        .map(ChannelId);

    if let Some(Channel::Guild(channel)) = channel_id.and_then(|c| c.to_channel(ctx).ok()) {
        let channel = channel.read();

        if let Ok(members) = channel.members(&ctx.cache) {
            for member in members {
                completion.add_with_options(
                    &format!("@{}", member.distinct()),
                    false,
                    CompletionPosition::Sorted,
                );
            }
        }
    }

    ReturnCode::Ok
}

fn handle_role_completion(buffer: &Buffer, completion: weechat::Completion) -> ReturnCode {
    let ctx = match discord::get_ctx() {
        Some(s) => s,
        None => return ReturnCode::Ok,
    };

    let guild = buffer
        .get_localvar("guildid")
        .and_then(|id| id.parse().ok())
        .map(GuildId);

    if let Some(guild) = guild {
        if let Some(guild) = guild.to_guild_cached(&ctx.cache) {
            let roles = &guild.read().roles;
            for role in roles.values() {
                completion.add(&format!("@{}", role.name));
            }
        }
    }

    ReturnCode::Ok
}

// TODO: Make this faster
// TODO: Handle command options
pub fn handle_query(command: &str) -> ReturnCode {
    if command.len() <= "/query ".len() {
        plugin_print("query requires a username");
        return ReturnCode::Ok;
    }

    let owned_cmd = command.to_owned();
    thread::spawn(move || {
        let ctx = match crate::discord::get_ctx() {
            Some(ctx) => ctx,
            _ => return,
        };
        let current_user = &ctx.cache.read().user;
        let substr = &owned_cmd["/query ".len()..].trim();

        let mut found_members: Vec<User> = Vec::new();
        for private_channel in ctx.cache.read().private_channels.values() {
            if private_channel
                .read()
                .name()
                .to_lowercase()
                .contains(&substr.to_lowercase())
            {
                found_members.push(private_channel.read().recipient.read().clone())
            }
        }

        if found_members.is_empty() {
            let guilds = current_user.guilds(ctx).expect("Unable to fetch guilds");
            for guild in &guilds {
                if let Some(guild) = guild.id.to_guild_cached(ctx) {
                    let guild = guild.read().clone();
                    for m in guild.members_containing(substr, false, true) {
                        found_members.push(m.user.read().clone());
                    }
                }
            }
        }
        found_members.dedup_by_key(|mem| mem.id);

        let current_user_name = current_user.name.clone();

        if let Some(target) = found_members.get(0) {
            if let Ok(chan) = target.create_dm_channel(ctx) {
                on_main(move |weechat| {
                    let ctx = match crate::discord::get_ctx() {
                        Some(ctx) => ctx,
                        _ => return,
                    };
                    crate::buffers::create_buffer_from_dm(
                        &ctx.cache,
                        &weechat,
                        Channel::Private(Arc::new(RwLock::new(chan))),
                        &current_user_name,
                        true,
                    );
                });
                return;
            }
        }

        plugin_print(&format!("Could not find user {:?}", substr));
    });
    ReturnCode::OkEat
}

// TODO: Handle command options
fn handle_nick(buffer: &Buffer, command: &str) -> ReturnCode {
    if buffer.get_localvar("guildid").is_none() {
        return ReturnCode::Ok;
    };

    let guilds;
    let mut substr;
    {
        let ctx = match crate::discord::get_ctx() {
            Some(ctx) => ctx,
            _ => return ReturnCode::Error,
        };
        substr = command["/nick".len()..].trim().to_owned();
        let mut split = substr.split(' ');
        let all = split.next() == Some("-all");
        if all {
            substr = substr["-all".len()..].trim().to_owned();
        }
        guilds = if all {
            let current_user = &ctx.cache.read().user;

            // TODO: Error handling
            current_user
                .guilds(ctx)
                .unwrap_or_default()
                .iter()
                .map(|g| g.id)
                .collect()
        } else {
            let guild = buffer
                .get_localvar("guildid")
                .expect("must to be some, checked at top of function");
            let guild = match guild.parse::<u64>() {
                Ok(v) => GuildId(v),
                Err(_) => return ReturnCode::OkEat,
            };
            vec![guild]
        };
    }

    thread::spawn(move || {
        {
            let ctx = match crate::discord::get_ctx() {
                Some(ctx) => ctx,
                _ => return,
            };
            let should_sleep = guilds.len() > 1;
            for guild in guilds {
                let new_nick = if substr.is_empty() {
                    None
                } else {
                    Some(substr.as_str())
                };
                let _ = guild.edit_nickname(ctx, new_nick);
                // Make it less spammy
                if should_sleep {
                    thread::sleep(Duration::from_secs(1));
                }
            }
        }
    });
    ReturnCode::OkEat
}

fn handle_join(buffer: &Buffer, command: &str) -> ReturnCode {
    let verbose = buffer.get_localvar("guildid").is_some();

    crate::command::join(
        &buffer.get_weechat(),
        crate::command::Args::from_cmd(&format!("/discord {}", &command[1..])),
        verbose,
    )
}
