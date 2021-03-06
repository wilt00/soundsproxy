use chrono::{DateTime, FixedOffset, TimeZone};
use futures::try_join;
use hhmmss::Hhmmss;
use hyper::service::{make_service_fn, service_fn};
use hyper::{header, Body, Method, Request, Response, Server, StatusCode};
use rss::extension::itunes::{ITunesChannelExtensionBuilder, ITunesItemExtensionBuilder};
use rss::{ChannelBuilder, EnclosureBuilder, ItemBuilder};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::time::Duration;

type GenericError = Box<dyn std::error::Error + Send + Sync>;
type ApiResult<T> = std::result::Result<T, GenericError>;

const PORT: u16 = 8223;

#[derive(Debug, Deserialize, Serialize)]
struct PodContainer {
    titles: PodTitles,
    synopses: PodSynopses,
    image_url: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct PodSynopses {
    #[serde(deserialize_with = "serde_with::rust::default_on_null::deserialize")]
    short: String,
    #[serde(deserialize_with = "serde_with::rust::default_on_null::deserialize")]
    medium: String,
    #[serde(deserialize_with = "serde_with::rust::default_on_null::deserialize")]
    long: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct PodEpisodes {
    data: Vec<PodEpisode>,
}

#[derive(Debug, Deserialize, Serialize)]
struct PodEpisode {
    titles: PodTitles,
    synopses: PodSynopses,
    image_url: String,
    duration: PodDuration,
    download: PodDownload,
    release: PodRelease,
}

#[derive(Debug, Deserialize, Serialize)]
struct PodTitles {
    #[serde(deserialize_with = "serde_with::rust::default_on_null::deserialize")]
    primary: String,
    #[serde(deserialize_with = "serde_with::rust::default_on_null::deserialize")]
    secondary: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct PodDuration {
    value: u64,
    label: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct PodDownload {
    #[serde(rename = "type")]
    download_type: String, // "non-drm"
    quality_variants: PodQualityVariants,
}

#[derive(Debug, Deserialize, Serialize)]
struct PodQualityVariants {
    low: PodQualityVariant,
    medium: PodQualityVariant,
    high: PodQualityVariant,
}

#[derive(Debug, Deserialize, Serialize)]
struct PodQualityVariant {
    bitrate: u32,
    file_url: String,
    file_size: u32,
    label: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct PodRelease {
    date: String,
    label: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct PodError {
    id: String,
    href: String,
    status: u32,
    message: String,
    replied_at: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct PodErrors {
    errors: Vec<PodError>,
}

#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum PodContainerResponse {
    Success(PodContainer),
    Failure(PodErrors),
}

#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum PodEpisodesResponse {
    Success(PodEpisodes),
    Failure(PodErrors),
}

async fn get_pod_info(id: &str) -> Result<PodContainerResponse, reqwest::Error> {
    let url = format!("https://rms.api.bbc.co.uk/v2/programmes/{}/container", id);
    let client = reqwest::Client::builder()
        .user_agent("soundsproxy/0.1")
        .build()?;
    client
        .get(url)
        .send()
        .await?
        .json::<PodContainerResponse>()
        .await
}

async fn get_pod_episodes(id: &str) -> Result<PodEpisodesResponse, reqwest::Error> {
    let url = format!(
        "https://rms.api.bbc.co.uk/v2/programmes/playable?container={}&sort=sequential&type=episode&experience=domestic",
         id);
    let client = reqwest::Client::builder()
        .user_agent("soundsproxy/0.1")
        .build()?;
    client
        .get(url)
        .send()
        .await?
        .json::<PodEpisodesResponse>()
        .await
}

fn replace_img_url(input: &str) -> String {
    input.replace("{recipe}", "288x288")
}

fn build_rss(id: &str, info: &PodContainer, episodes: &PodEpisodes) -> String {
    let items: Vec<rss::Item> = episodes
        .data
        .iter()
        .map(|e| {
            let encl = EnclosureBuilder::default()
                .mime_type("audio/mpeg".to_string())
                .length(e.download.quality_variants.high.file_size.to_string())
                .url(e.download.quality_variants.high.file_url.clone())
                .build();
            let itunes_ext = ITunesItemExtensionBuilder::default()
                .image(replace_img_url(&e.image_url))
                .duration(Duration::new(e.duration.value, 0).hhmmss())
                .subtitle(e.synopses.short.clone())
                .build();
            ItemBuilder::default()
                .title(e.titles.secondary.clone())
                .description(e.synopses.long.clone())
                .itunes_ext(itunes_ext)
                .enclosure(encl)
                .pub_date(
                    DateTime::parse_from_rfc3339(&e.release.date)
                        .unwrap_or_else(|_| FixedOffset::east(0).timestamp(0, 0))
                        .to_rfc2822(),
                )
                .build()
        })
        .collect();
    let mut namespaces: BTreeMap<String, String> = BTreeMap::new();
    namespaces.insert(
        "itunes".to_string(),
        "http://www.itunes.com/dtds/podcast-1.0.dtd".to_string(),
    );
    namespaces.insert(
        "content".to_string(),
        "http://purl.org/rss/1.0/modules/content/".to_string(),
    );
    let itunes_channel = ITunesChannelExtensionBuilder::default()
        .author("BBC".to_string())
        .block("Yes".to_string())
        .image(replace_img_url(&info.image_url))
        .complete("No".to_string())
        .build();
    let channel = ChannelBuilder::default()
        .namespaces(namespaces)
        .title(info.titles.primary.clone())
        .description(info.synopses.medium.clone())
        .itunes_ext(itunes_channel)
        .link(format!("https://www.bbc.co.uk/sounds/series/{}", id))
        .items(items)
        .build();
    channel.to_string()
}

async fn get_feed(path: &str) -> Response<Body> {
    let id = path[1..].to_string();
    match try_join!(get_pod_info(&id), get_pod_episodes(&id)) {
        Ok((PodContainerResponse::Success(info), PodEpisodesResponse::Success(episodes))) => {
            let rss = build_rss(&id, &info, &episodes);
            Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, "application/rss+xml")
                .body(Body::from(rss))
                .unwrap()
        }
        Ok((PodContainerResponse::Failure(err), _))
        | Ok((_, PodEpisodesResponse::Failure(err))) => Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Body::from(err.errors[0].message.clone()))
            .unwrap(),
        Err(e) => Response::builder()
            .status(StatusCode::BAD_GATEWAY)
            .body(Body::from(e.to_string()))
            .unwrap(),
    }
}

async fn router(req: Request<Body>) -> ApiResult<Response<Body>> {
    match (req.method(), req.uri().path()) {
        (&Method::GET, "/") => Ok(Response::new("Hello, World".into())),
        (&Method::GET, p) => Ok(get_feed(p).await),
        (_, _) => Ok(Response::new("Hello, World".into())),
    }
}

async fn shutdown_signal() {
    // Wait for the CTRL+C signal
    tokio::signal::ctrl_c()
        .await
        .expect("failed to install CTRL+C signal handler");
}

#[tokio::main]
async fn main() {
    let addr = SocketAddr::from(([127, 0, 0, 1], PORT));
    // let client = reqwest::Client::builder()
    //     .user_agent("soundsproxy/0.1")
    //     .build()
    //     .unwrap();

    let svc = make_service_fn(move |_| {
        // let c = client.clone();
        async { Ok::<_, GenericError>(service_fn(move |req| router(req))) }
    });
    let srv = Server::bind(&addr).serve(svc);

    let graceful = srv.with_graceful_shutdown(shutdown_signal());

    if let Err(e) = graceful.await {
        eprintln!("server error: {}", e);
    }
}
