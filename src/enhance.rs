//! Convoy, VirtFusion and Enhance modules, written against their public API
//! docs (Convoy application API, VirtFusion API v1, Enhance OpenAPI v12).
//! Covered by in-process mock servers only; not exercised against a live
//! panel.
use pelty_provisioning::http::call;
use pelty_provisioning::http::Method;
use pelty_provisioning::util::{gen_password, gone_ok, md};
use pelty_provisioning::{
    cfg_u64, fld, remote_id, BoxFuture, Created, ModuleInfo, ProvisionError, ProvisionResult,
    ProvisioningModule, ServerConn, ServiceCtx,
};
use serde_json::{json, Value};

const J: &str = "application/json";

fn safe_seg(s: &str) -> ProvisionResult<&str> {
    if !s.is_empty()
        && s.len() <= 100
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        Ok(s)
    } else {
        Err(ProvisionError::Config("invalid remote id".into()))
    }
}

fn id_str(v: Option<&Value>) -> Option<String> {
    v.and_then(|x| {
        x.as_str()
            .map(str::to_string)
            .or_else(|| x.as_u64().map(|n| n.to_string()))
    })
    .filter(|s| !s.is_empty())
}

fn display_name(svc: &ServiceCtx) -> String {
    let n = format!("{} {}", svc.customer_first_name, svc.customer_last_name)
        .trim()
        .to_string();
    if n.is_empty() {
        svc.customer_email.clone()
    } else {
        n
    }
}

/// Server config: `org_id` = the reseller/master organisation id.
#[derive(Default)]
pub struct Enhance;

fn master_org(server: &ServerConn) -> ProvisionResult<&str> {
    safe_seg(
        server
            .config
            .get("org_id")
            .and_then(Value::as_str)
            .ok_or_else(|| ProvisionError::Config("server config needs org_id".into()))?,
    )
}

impl ProvisioningModule for Enhance {
    fn info(&self) -> ModuleInfo {
        ModuleInfo {
            id: "enhance",
            name: "Enhance",
            server_fields: vec![fld!("org_id", "text", true)],
            product_fields: vec![fld!("plan_id", "number", true)],
            actions: vec![],
        }
    }
    fn test_connection<'a>(&'a self, server: &'a ServerConn) -> BoxFuture<'a, ProvisionResult<()>> {
        Box::pin(async move {
            call(
                server,
                Method::GET,
                &format!("/api/orgs/{}", master_org(server)?),
                J,
                None,
            )
            .await
            .map(|_| ())
        })
    }
    fn create<'a>(
        &'a self,
        server: &'a ServerConn,
        svc: &'a ServiceCtx,
    ) -> BoxFuture<'a, ProvisionResult<Created>> {
        Box::pin(async move {
            if let Some(id) = &svc.remote_id {
                return Ok(Created {
                    remote_id: id.clone(),
                    module_data: svc.module_data.clone(),
                });
            }
            let org = master_org(server)?;
            let r = call(
                server,
                Method::POST,
                &format!("/api/orgs/{org}/customers"),
                J,
                Some(&json!({ "name": display_name(svc) })),
            )
            .await?;
            let cust = id_str(r.get("id")).ok_or_else(|| {
                ProvisionError::Transient("Enhance returned no customer id".into())
            })?;
            let cust = safe_seg(&cust)?.to_string();
            let pw = gen_password(16);
            let r = call(server, Method::POST, &format!("/api/logins?orgId={cust}"), J, Some(&json!({ "email": svc.customer_email, "name": display_name(svc), "password": pw }))).await?;
            let login = id_str(r.get("id"))
                .ok_or_else(|| ProvisionError::Transient("Enhance returned no login id".into()))?;
            let r = call(
                server,
                Method::POST,
                &format!("/api/orgs/{cust}/members"),
                J,
                Some(&json!({ "loginId": login, "roles": ["Owner"] })),
            )
            .await?;
            let member = id_str(r.get("id"));
            let r = call(
                server,
                Method::POST,
                &format!("/api/orgs/{org}/customers/{cust}/subscriptions"),
                J,
                Some(&json!({ "planId": cfg_u64(&svc.product_config, "plan_id")? })),
            )
            .await?;
            let sub = id_str(r.get("id")).ok_or_else(|| {
                ProvisionError::Transient("Enhance returned no subscription id".into())
            })?;
            if let Ok(dom) = pelty_provisioning::util::domain(svc) {
                call(server, Method::POST, &format!("/api/orgs/{cust}/websites"), J, Some(&json!({ "domain": dom, "subscriptionId": sub.parse::<u64>().unwrap_or(0) }))).await?;
            }
            Ok(Created {
                remote_id: cust.clone(),
                module_data: json!({ "customer_id": cust, "subscription_id": sub, "member_id": member, "initial_password": pw }),
            })
        })
    }
    fn suspend<'a>(
        &'a self,
        server: &'a ServerConn,
        svc: &'a ServiceCtx,
    ) -> BoxFuture<'a, ProvisionResult<()>> {
        Box::pin(async move { enhance_suspend(server, svc, true).await })
    }
    fn unsuspend<'a>(
        &'a self,
        server: &'a ServerConn,
        svc: &'a ServiceCtx,
    ) -> BoxFuture<'a, ProvisionResult<()>> {
        Box::pin(async move { enhance_suspend(server, svc, false).await })
    }
    fn terminate<'a>(
        &'a self,
        server: &'a ServerConn,
        svc: &'a ServiceCtx,
    ) -> BoxFuture<'a, ProvisionResult<()>> {
        Box::pin(async move {
            gone_ok(
                call(
                    server,
                    Method::DELETE,
                    &format!("/api/orgs/{}?force=true", safe_seg(remote_id(svc)?)?),
                    J,
                    None,
                )
                .await,
            )
        })
    }
    fn change_package<'a>(
        &'a self,
        server: &'a ServerConn,
        svc: &'a ServiceCtx,
    ) -> BoxFuture<'a, ProvisionResult<()>> {
        Box::pin(async move {
            let (cust, sub) = (
                safe_seg(remote_id(svc)?)?,
                safe_seg(md(svc, "subscription_id")?)?,
            );
            call(
                server,
                Method::PATCH,
                &format!("/api/orgs/{cust}/subscriptions/{sub}"),
                J,
                Some(&json!({ "planId": cfg_u64(&svc.product_config, "plan_id")? })),
            )
            .await
            .map(|_| ())
        })
    }
    fn sso_url<'a>(
        &'a self,
        server: &'a ServerConn,
        svc: &'a ServiceCtx,
        _token: &'a str,
    ) -> BoxFuture<'a, ProvisionResult<Option<String>>> {
        Box::pin(async move {
            let (cust, member) = (safe_seg(remote_id(svc)?)?, safe_seg(md(svc, "member_id")?)?);
            let r = call(
                server,
                Method::GET,
                &format!("/api/orgs/{cust}/members/{member}/sso"),
                J,
                None,
            )
            .await?;
            Ok(r.as_str().map(str::to_string))
        })
    }
}

async fn enhance_suspend(server: &ServerConn, svc: &ServiceCtx, on: bool) -> ProvisionResult<()> {
    let (cust, sub) = (
        safe_seg(remote_id(svc)?)?,
        safe_seg(md(svc, "subscription_id")?)?,
    );
    call(
        server,
        Method::PATCH,
        &format!("/api/orgs/{cust}/subscriptions/{sub}"),
        J,
        Some(&json!({ "isSuspended": on })),
    )
    .await
    .map(|_| ())
}
