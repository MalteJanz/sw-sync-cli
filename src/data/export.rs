use crate::data::transform::serialize_entity;
use crate::SyncContext;
use std::cmp;
use std::sync::Arc;
use tokio::sync::mpsc;

pub async fn export(context: Arc<SyncContext>) -> anyhow::Result<()> {
    let total = context.sw_client.get_total(&context.schema.entity).await?;

    let chunk_limit = cmp::min(cmp::min(500, context.limit.unwrap_or(500)), total);
    let mut page = 1;
    let mut counter = 0;
    println!(
        "Reading {} of entity '{}' with chunk limit {}",
        total, context.schema.entity, chunk_limit
    );

    // start writer task
    let (writer_tx, writer_rx) = mpsc::channel::<WriterMessage>(64);
    let writer_context = Arc::clone(&context);
    let writer_task =
        tokio::task::spawn_blocking(
            || async move { write_to_file(writer_rx, &writer_context).await },
        );

    // submit request tasks
    let mut request_tasks = vec![];
    loop {
        if counter >= total {
            break;
        }

        let writer_tx = writer_tx.clone();
        let context = Arc::clone(&context);
        request_tasks.push(tokio::spawn(async move {
            process_request(page, chunk_limit, writer_tx, &context).await
        }));

        page += 1;
        counter += chunk_limit;
    }
    drop(writer_tx);

    // wait for all request tasks to finish
    for handle in request_tasks {
        handle.await??;
    }

    // wait for writer to finish
    writer_task.await?.await?;

    Ok(())
}

#[derive(Debug, Clone)]
struct WriterMessage {
    page: u64,
    rows: Vec<Vec<String>>,
}

async fn write_to_file(
    mut writer_rx: mpsc::Receiver<WriterMessage>,
    context: &SyncContext,
) -> anyhow::Result<()> {
    let mut csv_writer = csv::WriterBuilder::new()
        .delimiter(b';')
        .from_path(&context.file)?;

    // writer header line
    csv_writer.write_record(get_header_line(context))?;

    let mut next_write_page = 1;
    let mut buffer: Vec<WriterMessage> = Vec::with_capacity(64);
    while let Some(msg) = writer_rx.recv().await {
        buffer.push(msg);
        buffer.sort_unstable_by(|a, b| a.page.cmp(&b.page));

        while let Some(first) = buffer.first() {
            if first.page != next_write_page {
                break; // need to wait for receiving the correct page
            }

            let write_msg = buffer.remove(0);
            for row in write_msg.rows {
                csv_writer.write_record(row)?;
            }

            next_write_page += 1;
        }
    }

    csv_writer.flush()?;

    Ok(())
}

async fn process_request(
    page: u64,
    chunk_limit: u64,
    writer_tx: mpsc::Sender<WriterMessage>,
    context: &SyncContext,
) -> anyhow::Result<()> {
    println!(
        "fetching page {} of {} with limit {}",
        page, context.schema.entity, chunk_limit
    );
    let mut rows: Vec<Vec<String>> = Vec::with_capacity(chunk_limit as usize);

    let response = context
        .sw_client
        .list(&context.schema.entity, page, chunk_limit)
        .await?;
    for entity in response.data {
        let row = serialize_entity(entity, context)?;
        rows.push(row);
    }

    // submit it to write queue
    writer_tx.send(WriterMessage { page, rows }).await?;

    Ok(())
}

fn get_header_line(context: &SyncContext) -> Vec<String> {
    let mut columns = vec![];

    for mapping in &context.schema.mappings {
        columns.push(mapping.get_file_column().to_owned());
    }

    columns
}
