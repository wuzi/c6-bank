//! Handwritten C6 Pix request and response models. Monetary quantities stay strings.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

pub type ExtraFields = BTreeMap<String, Value>;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DueCalendar {
    pub data_de_vencimento: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub validade_apos_vencimento: Option<u32>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Debtor {
    pub nome: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpf: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cnpj: Option<String>,
    pub logradouro: String,
    pub cidade: String,
    pub uf: String,
    pub cep: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Adjustment {
    pub modalidade: u8,
    pub valor_perc: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct DueValue {
    pub original: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub multa: Option<Adjustment>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub juros: Option<Adjustment>,
    #[serde(flatten)]
    pub extra: ExtraFields,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DueChargeRequest {
    pub calendario: DueCalendar,
    pub devedor: Debtor,
    pub valor: DueValue,
    pub chave: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub solicitacao_pagador: Option<String>,
    #[serde(flatten)]
    pub extra: ExtraFields,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DueCharge {
    pub txid: String,
    pub status: String,
    pub calendario: Option<Value>,
    pub valor: Option<DueValue>,
    pub chave: Option<String>,
    pub pix_copia_e_cola: Option<String>,
    #[serde(default)]
    pub pix: Vec<Pix>,
    #[serde(flatten)]
    pub extra: ExtraFields,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Pix {
    pub end_to_end_id: String,
    pub txid: Option<String>,
    pub valor: String,
    pub chave: Option<String>,
    pub horario: String,
    pub componentes_valor: Option<Value>,
    #[serde(flatten)]
    pub extra: ExtraFields,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
/// Receipt query bounds are RFC3339 timestamps. [`crate::Client::list_pix`]
/// converts them to UTC and truncates fractional seconds for C6's query format.
pub struct PixQuery {
    pub inicio: String,
    pub fim: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub txid: Option<String>,
    #[serde(rename = "paginacao.paginaAtual")]
    pub pagina_atual: u32,
    #[serde(rename = "paginacao.itensPorPagina")]
    pub itens_por_pagina: u32,
}
impl PixQuery {
    pub(crate) fn normalized(&self) -> Result<Self, crate::Error> {
        let start = chrono::DateTime::parse_from_rfc3339(&self.inicio)
            .map_err(|_| crate::Error::Configuration)?;
        let end = chrono::DateTime::parse_from_rfc3339(&self.fim)
            .map_err(|_| crate::Error::Configuration)?;
        if start > end {
            return Err(crate::Error::Configuration);
        }
        Ok(Self {
            inicio: start
                .with_timezone(&chrono::Utc)
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            fim: end
                .with_timezone(&chrono::Utc)
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            ..self.clone()
        })
    }
}
impl Default for PixQuery {
    fn default() -> Self {
        Self {
            inicio: String::new(),
            fim: String::new(),
            txid: None,
            pagina_atual: 0,
            itens_por_pagina: 100,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PixPage {
    pub pix: Vec<Pix>,
    pub parametros: Option<PixParameters>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PixParameters {
    pub paginacao: Option<Pagination>,
    #[serde(flatten)]
    pub extra: ExtraFields,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Pagination {
    pub pagina_atual: u32,
    pub itens_por_pagina: u32,
    pub quantidade_de_paginas: u32,
    /// C6 may omit this count for received Pix, including nonempty pages.
    /// `None` means unknown, not zero; paginate using `quantidade_de_paginas`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quantidade_total_de_itens: Option<u64>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Webhook {
    pub webhook_url: String,
    pub chave: Option<String>,
    pub criacao: Option<String>,
    #[serde(flatten)]
    pub extra: ExtraFields,
}
