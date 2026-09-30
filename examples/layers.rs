//! Layers: retries, deadlines, and the caller's own code around every call.
//!
//! ```sh
//! SVIR_MODEL=<model> cargo run --example layers
//! ```

mod common;

use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use svir::layer::{Layer, Next, Retry, Timeout};
use svir::prelude::*;

/// Counts the tokens every answer of a client took to generate.
struct Meter {
    generated: Arc<AtomicU64>,
}

impl Layer for Meter {
    async fn call(&self, request: Request, next: Next) -> Result<EventStream, Error> {
        let generated = self.generated.clone();
        let stream = next.run(request).await?;

        // `next.run` resolves when the response starts. The answer is still to come, so the
        // layer watches the stream it hands back.
        Ok(stream.inspect(move |item| {
            if let Ok(Event::Completed(done)) = item {
                let tokens = done.usage.map_or(0, |usage| usage.output);
                generated.fetch_add(tokens, Ordering::Relaxed);
            }
        }))
    }
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    let generated = Arc::new(AtomicU64::new(0));

    // The first layer added is the outermost: a retry runs everything under it again, so the
    // closure at the bottom reports every attempt. Try it with SVIR_URL set to a closed port.
    let client = Client::openai(common::url())
        .layer(Retry::connect(3).backoff(Duration::from_millis(200)))
        .layer(Timeout::first_token(Duration::from_secs(120)))
        .layer(Meter {
            generated: generated.clone(),
        })
        .wrap(|request, next| async move {
            let started = Instant::now();
            let answer = next.run(request).await;

            match &answer {
                Ok(_) => eprintln!("-- the response started after {:?}", started.elapsed()),
                Err(error) => eprintln!("-- failed after {:?}: {error}", started.elapsed()),
            }
            answer
        })
        .build()?;

    let model = common::model();
    for question in ["Name a river in Karelia.", "Name a river in Portugal."] {
        let answer = client.complete(Request::new(&model).user(question)).await?;
        println!("{}", answer.text.trim());
    }
    eprintln!("-- {} tokens generated", generated.load(Ordering::Relaxed));

    Ok(())
}
