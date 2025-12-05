use reqwest::header::RANGE;
use tokio::io::AsyncWriteExt;

use crate::{
    InitialSegment, RemoteStreamingSegment, StreamingSegment, ToSegmentData, WriteSegment,
    context::IoriContext,
    error::{IoriError, IoriResult},
    util::http::HttpClient,
};

impl<T> ToSegmentData for T
where
    T: RemoteStreamingSegment,
{
    fn to_segment_data(
        &self,
        client: HttpClient,
    ) -> impl Future<Output = IoriResult<bytes::Bytes>> + Send {
        let url = self.url();
        let byte_range = self.byte_range();
        let headers = self.headers();
        async move {
            let mut request = client.get(url);
            if let Some(headers) = headers {
                request = request.headers(headers);
            }
            if let Some(byte_range) = byte_range {
                request = request.header(RANGE, byte_range.to_http_range());
            }
            let response = request.send().await?;
            if !response.status().is_success() {
                let status = response.status();
                if let Ok(body) = response.text().await {
                    tracing::warn!("Error body: {body}");
                }
                return Err(IoriError::HttpError(status));
            }

            let bytes = response.bytes().await?;
            Ok(bytes)
        }
    }
}

impl<T> WriteSegment for T
where
    T: StreamingSegment + RemoteStreamingSegment + Sync,
{
    async fn write_segment<W>(&self, context: &IoriContext, writer: &mut W) -> IoriResult<()>
    where
        W: tokio::io::AsyncWrite + Unpin + Send,
    {
        let bytes = self.to_segment_data(context.client.clone()).await?;

        // TODO: use bytes_stream to improve performance
        // .bytes_stream();
        let decryptor = self.key().map(|key| {
            key.to_decryptor(
                self.format(),
                context.shaka_packager_command.as_ref().to_owned(),
            )
        });
        if let Some(decryptor) = decryptor {
            let decrypted_bytes = match self.initial_segment() {
                crate::InitialSegment::Encrypted(data) => {
                    let mut result = data.to_vec();
                    result.extend_from_slice(&bytes);
                    decryptor.decrypt(&result).await?
                }
                crate::InitialSegment::Clear(data) => {
                    writer.write_all(&data).await?;
                    decryptor.decrypt(&bytes).await?
                }
                crate::InitialSegment::None => decryptor.decrypt(&bytes).await?,
            };
            writer.write_all(&decrypted_bytes).await?;
        } else {
            // If no key is provided, no matter whether the initial segment is encrypted or not,
            // we should write the initial segment to the file.
            if let InitialSegment::Clear(initial_segment)
            | InitialSegment::Encrypted(initial_segment) = self.initial_segment()
            {
                writer.write_all(&initial_segment).await?;
            }
            writer.write_all(&bytes).await?;
        }
        writer.flush().await?;

        Ok(())
    }
}
