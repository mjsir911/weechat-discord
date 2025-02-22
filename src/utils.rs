use indexmap::IndexMap;
use lazy_static::lazy_static;
use regex::Regex;
use serenity::{
    cache::Cache,
    cache::CacheRwLock,
    model::{id::ChannelId, prelude::*},
    prelude::*,
};
use std::sync::Arc;
use weechat::{Buffer, ConfigOption, Weechat};

#[derive(Debug, Clone, Copy)]
pub enum GuildOrChannel {
    Guild(GuildId),
    Channel(Option<GuildId>, ChannelId),
}

impl PartialEq<GuildId> for GuildOrChannel {
    fn eq(&self, other: &GuildId) -> bool {
        match self {
            GuildOrChannel::Guild(this_id) => this_id == other,
            GuildOrChannel::Channel(_, _) => false,
        }
    }
}

impl PartialEq<ChannelId> for GuildOrChannel {
    fn eq(&self, other: &ChannelId) -> bool {
        match self {
            GuildOrChannel::Guild(_) => false,
            GuildOrChannel::Channel(_, this_id) => this_id == other,
        }
    }
}

pub fn rgb_to_ansi(color: serenity::utils::Colour) -> u8 {
    let r = (u16::from(color.r()) * 5 / 255) as u8;
    let g = (u16::from(color.g()) * 5 / 255) as u8;
    let b = (u16::from(color.b()) * 5 / 255) as u8;
    16 + 36 * r + 6 * g + b
}

pub fn status_is_online(status: OnlineStatus) -> bool {
    use OnlineStatus::*;
    match status {
        Online | Idle | DoNotDisturb => true,
        Offline | Invisible => false,
        __Nonexhaustive => unreachable!(),
    }
}

pub fn get_user_status_prefix(weechat: &Weechat, cache: &Cache, user: UserId) -> String {
    let presence = cache.presences.get(&user);

    format_user_status_prefix(weechat, presence.map(|p| p.status))
}

pub fn format_user_status_prefix(weechat: &Weechat, status: Option<OnlineStatus>) -> String {
    let prefix_color = match status {
        Some(OnlineStatus::DoNotDisturb) => "red",
        Some(OnlineStatus::Idle) => "178",
        Some(OnlineStatus::Invisible) => "weechat.color.nicklist_away",
        Some(OnlineStatus::Offline) => "weechat.color.nicklist_away",
        Some(OnlineStatus::Online) => "green",
        _ => "".into(),
    };

    format!(
        "{}•{} ",
        weechat.color(prefix_color),
        weechat.color("Reset"),
    )
}

pub trait ChannelExt {
    fn name(&self) -> String;
    fn last_message(&self) -> Option<MessageId>;
}

impl ChannelExt for Channel {
    fn name(&self) -> String {
        use std::borrow::Cow;
        use Channel::*;
        match self {
            Guild(channel) => channel.read().name().to_string(),
            Group(channel) => match channel.read().name() {
                Cow::Borrowed(name) => name.to_string(),
                Cow::Owned(name) => name,
            },
            Category(category) => category.read().name().to_string(),
            Private(channel) => channel.read().name(),
            __Nonexhaustive => unreachable!(),
        }
    }

    fn last_message(&self) -> Option<MessageId> {
        use Channel::*;
        match self {
            Guild(channel) => channel.read().last_message_id,
            Group(channel) => channel.read().last_message_id,
            Category(_) => None,
            Private(channel) => channel.read().last_message_id,
            __Nonexhaustive => unreachable!(),
        }
    }
}

/// Find the highest hoisted role (used for the user group) and the highest role (used for user coloring)
pub fn find_highest_roles(cache: &CacheRwLock, member: &Member) -> Option<(Role, Role)> {
    let mut roles = member.roles(cache)?;
    roles.sort();
    let highest = roles.last();

    let highest_hoisted = roles.iter().filter(|role| role.hoist).collect::<Vec<_>>();
    let highest_hoisted = highest_hoisted.last().cloned();
    Some((highest_hoisted?.clone(), highest?.clone()))
}

pub fn unique_id(guild: Option<GuildId>, channel: ChannelId) -> String {
    if let Some(guild) = guild {
        format!("G{:?}C{}", guild.0, channel.0)
    } else {
        format!("C{}", channel.0)
    }
}

pub fn unique_guild_id(guild: GuildId) -> String {
    format!("G{}", guild)
}

pub fn parse_id(id: &str) -> Option<GuildOrChannel> {
    // id has channel part
    if let Some(c_start) = id.find('C') {
        if id.starts_with('C') {
            let channel_id = id[1..].parse().ok()?;
            Some(GuildOrChannel::Channel(None, channel_id))
        } else {
            let guild_id = id[1..c_start].parse().ok()?;
            let channel_id = id[c_start + 1..].parse().ok()?;
            Some(GuildOrChannel::Channel(Some(GuildId(guild_id)), channel_id))
        }
    } else {
        // id is only a guild
        let guild_id = id[1..].parse().ok()?;
        Some(GuildOrChannel::Guild(GuildId(guild_id)))
    }
}

pub fn get_irc_mode(weechat: &weechat::Weechat) -> bool {
    crate::upgrade_plugin(weechat).config.irc_mode.value()
}

pub fn buffer_id_for_guild(id: GuildId) -> String {
    format!("{}", id.0)
}

pub fn buffer_id_for_channel(guild_id: Option<GuildId>, channel_id: ChannelId) -> String {
    if let Some(guild_id) = guild_id {
        format!("{}.{}", guild_id, channel_id.0)
    } else {
        format!("Private.{}", channel_id.0)
    }
}

pub unsafe fn buffer_from_ptr(buffer_ptr: *mut std::ffi::c_void) -> Buffer {
    Buffer::from_ptr(
        crate::__PLUGIN.as_mut().unwrap().weechat.as_ptr(),
        buffer_ptr as *mut _,
    )
}

pub fn buffer_is_muted(buffer: &Buffer) -> bool {
    if let Some(muted) = buffer.get_localvar("muted") {
        muted == "1"
    } else {
        false
    }
}

pub fn search_channel(
    cache: &CacheRwLock,
    guild_name: &str,
    channel_name: &str,
) -> Option<(Arc<RwLock<Guild>>, Arc<RwLock<GuildChannel>>)> {
    if let Some(raw_guild) = search_guild(cache, guild_name) {
        let guild = raw_guild.read();
        for channel in guild.channels.values() {
            let channel_lock = channel.read();
            if parsing::weechat_arg_strip(&channel_lock.name).to_lowercase()
                == channel_name.to_lowercase()
                || channel_lock.id.0.to_string() == channel_name
            {
                // Skip non text channels
                use serenity::model::channel::ChannelType::*;
                match channel_lock.kind {
                    Text | Private | Group | News => {}
                    _ => continue,
                }
                return Some((raw_guild.clone(), channel.clone()));
            }
        }
    }
    None
}

pub fn search_guild(cache: &CacheRwLock, guild_name: &str) -> Option<Arc<RwLock<Guild>>> {
    for guild in cache.read().guilds.values() {
        let guild_lock = guild.read();
        if parsing::weechat_arg_strip(&guild_lock.name).to_lowercase() == guild_name.to_lowercase()
            || guild_lock.id.0.to_string() == guild_name
        {
            return Some(guild.clone());
        }
    }
    None
}

/// Take a slice of GuildOrChannel's and flatten it into a map of channels
pub fn flatten_guilds(
    ctx: &Context,
    items: &[GuildOrChannel],
) -> IndexMap<Option<GuildId>, Vec<ChannelId>> {
    let mut channels: IndexMap<Option<GuildId>, Vec<ChannelId>> = IndexMap::new();
    // flatten guilds into channels
    for item in items {
        match item {
            GuildOrChannel::Guild(guild_id) => {
                let guild_channels = guild_id.channels(ctx).unwrap_or_default();
                let mut guild_channels = guild_channels.values().collect::<Vec<_>>();
                guild_channels.sort_by_key(|g| g.position);
                channels
                    .entry(Some(*guild_id))
                    .or_default()
                    .extend(guild_channels.iter().map(|ch| ch.id));
            }
            GuildOrChannel::Channel(guild, channel) => {
                channels.entry(*guild).or_default().push(*channel);
            }
        }
    }

    channels
}

pub fn get_users_nth_message(
    ctx: &Context,
    channel: ChannelId,
    n: usize,
) -> serenity::Result<Message> {
    if n > 50 {
        return Err(serenity::Error::ExceededLimit(
            "Cannot fetch more than 50 items".into(),
            n as u32,
        ));
    }
    let user = ctx.cache.read().user.id;
    // TODO: Page if needed
    channel
        .messages(&ctx.http, |retriever| retriever.limit(50))
        .and_then(|msgs| {
            msgs.iter()
                .filter(|msg| msg.author.id == user)
                .nth(n - 1)
                .cloned()
                .ok_or(serenity::Error::Model(
                    serenity::model::ModelError::ItemMissing,
                ))
        })
}

// TODO: Role mentions
/// Parse user input and replace mentions with Discords internal representation
///
/// This is not in `parsing` because it depends on `serenity`
pub fn create_mentions(cache: &CacheRwLock, guild_id: Option<GuildId>, input: &str) -> String {
    let mut out = String::from(input);

    lazy_static! {
        static ref CHANNEL_MENTION: Regex = Regex::new(r"#([a-z_-]+)").unwrap();
        static ref USER_MENTION: Regex = Regex::new(r"@(.{0,32}?)#(\d{2,4})").unwrap();
    }

    let channel_mentions = CHANNEL_MENTION.captures_iter(input);
    for channel_match in channel_mentions {
        let channel_name = channel_match.get(1).unwrap().as_str();

        // TODO: Remove duplication
        if let Some(guild) = guild_id.and_then(|g| g.to_guild_cached(cache)) {
            for (id, chan) in &guild.read().channels {
                if chan.read().name() == channel_name {
                    out = out.replace(channel_match.get(0).unwrap().as_str(), &id.mention());
                }
            }
        } else {
            for (id, chan) in &cache.read().channels {
                if chan.read().name() == channel_name {
                    out = out.replace(channel_match.get(0).unwrap().as_str(), &id.mention());
                }
            }
        };
    }

    let user_mentions = USER_MENTION.captures_iter(input);
    // TODO: Support nick names
    for user_match in user_mentions {
        let user_name = user_match.get(1).unwrap().as_str();

        if let Some(guild) = guild_id.and_then(|g| g.to_guild_cached(cache)) {
            for (id, member) in &guild.read().members {
                if let Some(nick) = &member.nick {
                    if nick == user_name {
                        out = out.replace(user_match.get(0).unwrap().as_str(), &id.mention());
                        continue;
                    }
                }

                if member.user.read().name == user_name {
                    out = out.replace(user_match.get(0).unwrap().as_str(), &id.mention());
                }
            }
        }
        for (id, user) in cache.read().users.iter() {
            if user.read().name == user_name {
                out = out.replace(user_match.get(0).unwrap().as_str(), &id.mention());
            }
        }
    }

    out
}
