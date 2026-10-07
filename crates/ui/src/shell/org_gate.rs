//! The organization gate.

use super::*;

impl Shell {
    pub(super) fn ensure_org_ui(&mut self, cx: &mut Context<Self>) {
        if self.org.is_some() {
            return;
        }
        self.org = Some(OrgGateUi {
            orgs: Loadable::Idle,
            submitting: true,
            error: None,
            task: None,
        });
        self.load_orgs(cx);
    }

    pub(super) fn load_orgs(&mut self, cx: &mut Context<Self>) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let Some(org) = self.org.as_mut() else { return };
        org.orgs = Loadable::Loading;
        org.submitting = true;
        org.error = None;
        org.task = Some(cx.spawn(async move |this, cx| {
            let result: Result<Option<Vec<OrgRow>>, String> = async {
                let value = engine
                    .client()
                    .call(methods::LIST_ORGS, serde_json::json!({}))
                    .await
                    .map_err(|err| err.to_string())?;
                match org_setup(parse_orgs(&value)) {
                    OrgSetup::AutoCreate => {
                        engine
                            .client()
                            .call(
                                methods::CREATE_ORG,
                                serde_json::json!({ "name": DEFAULT_PERSONAL_ORG_NAME }),
                            )
                            .await
                            .map_err(|err| err.to_string())?;
                        Ok(None)
                    }
                    OrgSetup::AutoSelect(organization_id) => {
                        engine
                            .client()
                            .call(
                                methods::SELECT_ORG,
                                serde_json::json!({
                                    "organizationId": organization_id
                                }),
                            )
                            .await
                            .map_err(|err| err.to_string())?;
                        Ok(None)
                    }
                    OrgSetup::Pick(rows) => Ok(Some(rows)),
                }
            }
            .await;
            this.update(cx, |shell, cx| {
                if let Some(org) = shell.org.as_mut() {
                    match result {
                        Ok(Some(rows)) => {
                            org.orgs = Loadable::Ready(rows);
                            org.submitting = false;
                        }
                        Ok(None) => {
                            // CREATE_ORG and SELECT_ORG both re-scope auth. Keep
                            // the progress state visible until AuthStatus flips
                            // to SignedIn and removes the gate.
                            org.orgs = Loadable::Loading;
                        }
                        Err(err) => {
                            org.orgs = Loadable::Error(err);
                            org.submitting = false;
                        }
                    }
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    pub(super) fn select_org(&mut self, organization_id: String, cx: &mut Context<Self>) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let Some(org) = self.org.as_mut() else { return };
        org.submitting = true;
        org.error = None;
        org.task = Some(cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(
                    methods::SELECT_ORG,
                    serde_json::json!({ "organizationId": organization_id }),
                )
                .await;
            this.update(cx, |shell, cx| {
                if let Some(org) = shell.org.as_mut() {
                    org.submitting = false;
                    if let Err(err) = result {
                        org.error = Some(format!("{err}").into());
                    }
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }
}
