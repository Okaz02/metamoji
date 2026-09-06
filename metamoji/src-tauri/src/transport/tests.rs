use super::*;

use crate::test_support::stub;

fn client(root: &str) -> CloudClient {
    let client =
        CloudClient::new("test".into(), "ja_JP".into(), "Asia/Tokyo".into(), None).expect("client");
    client.set_root_server(root);
    client
}

fn get(url: &str) -> FetchRequest {
    FetchRequest {
        url: url.to_string(),
        method: "GET".into(),
        headers: HashMap::new(),
        body: None,
    }
}

fn decode(body: &str) -> String {
    String::from_utf8(
        base64::engine::general_purpose::STANDARD
            .decode(body)
            .expect("base64"),
    )
    .expect("utf-8")
}

#[tokio::test]
async fn hands_back_the_status_headers_and_body_as_they_came() {
    let server = stub(vec![(
        "200 OK",
        r#"{"serverURL":"https://mps101/"}"#.to_string(),
    )]);
    let cloud = client(&server.base);

    let response = fetch(
        &cloud,
        get(&format!("{}mpsroot/RequestServlet", server.base)),
    )
    .await
    .expect("fetch");

    assert_eq!(response.status, 200);
    assert_eq!(response.headers["content-type"], "application/json");
    assert_eq!(decode(&response.body), r#"{"serverURL":"https://mps101/"}"#);
}

#[tokio::test]
async fn keeps_set_cookie_out_of_the_header_map() {
    // A login sets several at once and a map holds one of them; the client up
    // there needs all of them, and separately from the rest.
    let server = stub(vec![("200 OK", "{}".to_string())]);
    let cloud = client(&server.base);

    let response = fetch(&cloud, get(&server.base)).await.expect("fetch");

    assert_eq!(response.set_cookie.len(), 1);
    assert!(response.set_cookie[0].contains("JSESSIONID=abc123"));
    assert!(!response.headers.contains_key("set-cookie"));
}

#[tokio::test]
async fn the_session_is_one_session_however_the_call_was_made() {
    // The whole reason this goes through Rust rather than the webview's own
    // fetch: a cookie taken on one call is presented on the next, whichever
    // side made either of them. Two jars would be two logins that drift.
    let server = stub(vec![
        ("200 OK", "{}".to_string()),
        ("200 OK", "{}".to_string()),
    ]);
    let cloud = client(&server.base);

    fetch(&cloud, get(&server.base)).await.expect("first");
    server.seen.recv().expect("first request");
    fetch(&cloud, get(&server.base)).await.expect("second");

    let second = server.seen.recv().expect("second request");
    assert_eq!(
        second.header("cookie"),
        Some("JSESSIONID=abc123"),
        "the second request presented what the first was given"
    );
}

#[tokio::test]
async fn a_request_body_goes_out_as_the_bytes_it_was_given() {
    let server = stub(vec![("200 OK", "{}".to_string())]);
    let cloud = client(&server.base);

    let payload = br#"{"userId":"1","password":null,"qwd":null}"#;
    fetch(
        &cloud,
        FetchRequest {
            url: format!("{}rest/users/login", server.base),
            method: "POST".into(),
            headers: HashMap::from([(
                "content-type".to_string(),
                "application/json; charset=utf-8".to_string(),
            )]),
            body: Some(base64::engine::general_purpose::STANDARD.encode(payload)),
        },
    )
    .await
    .expect("fetch");

    let seen = server.seen.recv().expect("request");
    assert_eq!(seen.method, "POST");
    assert_eq!(seen.body, String::from_utf8_lossy(payload));
    assert_eq!(
        seen.header("content-type"),
        Some("application/json; charset=utf-8")
    );
}

#[tokio::test]
async fn a_get_with_no_body_sends_none() {
    // Which commands send one is not uniform, and the client above decides.
    // This must not add one of its own.
    let server = stub(vec![("200 OK", "{}".to_string())]);
    let cloud = client(&server.base);

    fetch(&cloud, get(&format!("{}drives/entry", server.base)))
        .await
        .expect("fetch");

    let seen = server.seen.recv().expect("request");
    assert_eq!(seen.body, "");
    assert_eq!(seen.header("content-length"), None);
}

/// What `mpsroot/RequestServlet` answers, with the host swapped for the stub.
const SCHOOL_OK: &str = r#"{"isClassRoom":true,"serverURL":"{BASE}","errorCode":0}"#;

/// A lapsed session, verbatim from a live signed-out request. Note the 500 it
/// arrives with below: the status is no help, so the code is what is read.
const LAPSED: &str =
    r#"{"name":"NotLoginException","message":"It doesn't log it in.","data":{"errorCode":106}}"#;

fn login_ok() -> String {
    r#"{"uuid":"u-1","loginName":"student01","email":"a@example.jp","companyId":"c-1"}"#.to_string()
}

/// Signs a client in against the stub, consuming the first two replies.
async fn signed_in(server: &crate::test_support::Stub) -> CloudClient {
    let cloud = client(&server.base);
    cloud
        .login("school01", "student01", "hunter2")
        .await
        .expect("login");
    server.seen.recv().expect("school lookup");
    server.seen.recv().expect("login");
    cloud
}

#[tokio::test]
async fn a_lapsed_session_is_renewed_and_the_call_made_again() {
    // The client above has no credential and never will have one, so this is
    // the only place the recovery can happen. Without it a session that timed
    // out mid-lesson would surface as "It doesn't log it in." and stay there.
    let server = stub(vec![
        ("200 OK", SCHOOL_OK.to_string()),
        ("200 OK", login_ok()),
        ("500 Internal Server Error", LAPSED.to_string()),
        ("200 OK", login_ok()),
        ("200 OK", r#"{"errorCode":0,"joinCode":"1234"}"#.to_string()),
    ]);
    let cloud = signed_in(&server).await;

    let url = format!("{}users3/crbox/get/joincode", server.base);
    let response = fetch(&cloud, get(&url)).await.expect("fetch");

    assert_eq!(
        server.seen.recv().expect("first try").path,
        "/users3/crbox/get/joincode"
    );
    assert_eq!(
        server.seen.recv().expect("re-login").path,
        "/mmjeditor2/2.0/users3/login",
        "it signed in again rather than handing back the failure"
    );
    assert_eq!(
        server.seen.recv().expect("retry").path,
        "/users3/crbox/get/joincode"
    );
    assert_eq!(response.status, 200);
    assert_eq!(
        decode(&response.body),
        r#"{"errorCode":0,"joinCode":"1234"}"#
    );
}

#[tokio::test]
async fn some_other_refusal_is_handed_back_as_it_came() {
    // Only 106 means the session lapsed, and a 500 on its own means nothing —
    // signing in again for every server fault would turn one clear error into
    // two requests and a vaguer one.
    let server = stub(vec![
        ("200 OK", SCHOOL_OK.to_string()),
        ("200 OK", login_ok()),
        (
            "500 Internal Server Error",
            r#"{"errorCode":123,"errorMessage":"no"}"#.to_string(),
        ),
    ]);
    let cloud = signed_in(&server).await;

    let response = fetch(&cloud, get(&format!("{}users3/crbox/create", server.base)))
        .await
        .expect("fetch");

    assert_eq!(response.status, 500);
    assert_eq!(
        decode(&response.body),
        r#"{"errorCode":123,"errorMessage":"no"}"#
    );
    server.seen.recv().expect("the one request went out");
    assert!(server.seen.try_recv().is_err(), "and nothing followed it");
}

#[tokio::test]
async fn an_unknown_method_is_refused_rather_than_guessed() {
    let cloud = client("http://127.0.0.1:1/");
    let bad = FetchRequest {
        url: "http://127.0.0.1:1/".into(),
        method: "NOT A METHOD".into(),
        headers: HashMap::new(),
        body: None,
    };
    assert!(fetch(&cloud, bad).await.is_err());
}
