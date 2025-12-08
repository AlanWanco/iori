#[test]
fn test_accuracy() {
    let data = include_bytes!("./fixtures/radiko_01.m3u8");
    let old_result = iori_hls::m3u8_rs::parse_playlist_res(data);
    let new_result = iori_hls::parse::parse_playlist_res(data);

    let old_result = old_result.expect("Old parse engine should not error");
    let new_result = new_result.expect("New parse engine should not error");
    assert_eq!(old_result, new_result);
}
