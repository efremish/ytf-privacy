mod config;
mod library;
mod lock;
mod logfile;
mod media;
mod metadata;
mod pipeline;
mod plan;
mod report;
mod settings;
mod shorts;
mod tokens;
mod web;
mod youtube;

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "ytf", version, about = "YouTube type-beat pipeline: render + upload")]
struct Cli {
    /// корень проекта (по умолчанию — родительский каталог бинарника)
    #[arg(long, global = true)]
    root: Option<String>,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Проверить окружение: ffmpeg, структура папок, токены, квота
    Doctor,
    /// Показать найденные каналы и жанры
    Library,
    /// Проверить все токены: канал, scopes, живой ли
    Tokens,
    /// Импортировать токены из старого tokens.txt + client_secret.json
    Import,
    /// Получить новый refresh_token для папки канала
    Auth {
        /// имя папки канала в People/
        folder: String,
        /// имя канала на YouTube (для показа, необязательно)
        #[arg(long)]
        title: Option<String>,
    },
    /// Показать реальную квоту Google
    Quota,
    /// Разовая публикация по всем каналам (основная команда)
    Publish {
        /// канал (имя папки). Без флага — все каналы
        channel: Option<String>,
        /// принудительно рандомизировать теги и описание
        #[arg(long)]
        randomize: bool,
        /// горизонт дней вперёд
        #[arg(long, default_value_t = 1)]
        horizon: i64,
        /// сколько воркеров рендера
        #[arg(long)]
        workers: Option<usize>,
        /// отрендерить, но НЕ загружать на YouTube
        #[arg(long)]
        no_upload: bool,
    },
    /// Разовая публикация (синоним publish без указания канала)
    Dry {
        #[arg(long)]
        randomize: bool,
        #[arg(long, default_value_t = 1)]
        horizon: i64,
        #[arg(long)]
        workers: Option<usize>,
        #[arg(long)]
        no_upload: bool,
    },
    /// Залить готовые шортсы (без рендера). Общий пул Shorts/_pool/
    /// раскидывается по каналам; папки Shorts/<канал>/ — на свой канал.
    Shorts {
        /// канал (имя папки в Shorts/). Без флага — все
        channel: Option<String>,
        /// показать план, но ничего не загружать
        #[arg(long)]
        no_upload: bool,
        /// взять всю очередь, а не по одному на канал
        #[arg(long)]
        all: bool,
    },
    /// Проверить, считает ли система кадр чёрным (диагностика обложки)
    CheckThumb {
        /// путь к видео или к jpg
        file: String,
    },
    /// Залить свою обложку на конкретное видео (id + jpg)
    SetThumb {
        /// id видео из ссылки watch?v=ID
        video_id: String,
        /// путь к jpg (рекомендуется 1280x720)
        file: String,
    },
    /// Веб-интерфейс
    Web {
        #[arg(long, default_value_t = 3000)]
        port: u16,
        /// адрес для прослушивания; 0.0.0.0 откроет панель всей сети без пароля
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    rt.block_on(async move {
        let ctx = settings::Context::new(cli.root)?;
        // логи: консоль + logs/ytf-<дата>.log (ротация по дням)
        let log_path = logfile::init(&ctx.root);
        println!("логи: {}\n", log_path.display());

        let res = match cli.cmd {
            Cmd::Doctor => pipeline::doctor(&ctx).await,
            Cmd::Library => library::print_library(&ctx),
            Cmd::Tokens => tokens::check_all(&ctx).await,
            Cmd::Import => {
                let n = tokens::TokenStore::import_legacy(&ctx.root)?;
                println!("Импортировано каналов: {n}");
                println!("Файл: {}", ctx.tokens_file().display());
                Ok(())
            }
            Cmd::Auth { folder, title } => youtube::oauth::run(&ctx, &folder, title.as_deref()).await,
            Cmd::Shorts { channel, no_upload, all } => {
                shorts::publish(&ctx, channel.as_deref(), no_upload, all).await
            }
            Cmd::CheckThumb { file } => pipeline::check_thumb(&ctx, &file).await,
            Cmd::SetThumb { video_id, file } => pipeline::set_thumb(&ctx, &video_id, &file).await,
            Cmd::Quota => youtube::quota::print_quota(&ctx).await,
            Cmd::Publish { channel, randomize, horizon, workers, no_upload } => {
                pipeline::publish(&ctx, channel.as_deref(), randomize, horizon, workers, no_upload).await
            }
            Cmd::Dry { randomize, horizon, workers, no_upload } => {
                pipeline::publish(&ctx, None, randomize, horizon, workers, no_upload).await
            }
            Cmd::Web { port, host } => web::serve(&ctx, &host, port).await,
        };
        // любая ошибка верхнего уровня должна остаться в лог-файле
        if let Err(e) = &res {
            tracing::error!("{e:#}");
        }
        res
    })
}
