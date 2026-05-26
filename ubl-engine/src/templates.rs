use crate::error::UblError;
use minijinja::{context, Environment};

pub struct TemplateEngine {
    env: Environment<'static>,
}

impl Default for TemplateEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl TemplateEngine {
    pub fn new() -> Self {
        let mut env = Environment::new();
        let template_str = include_str!("../templates/invoice_template.xml");
        env.add_template("invoice", template_str)
            .expect("Failed to add invoice template");

        let att_doc_str = include_str!("../templates/attached_document_template.xml");
        env.add_template("attached_document", att_doc_str)
            .expect("Failed to add attached_document template");

        let app_resp_str = include_str!("../templates/application_response_template.xml");
        env.add_template("application_response", app_resp_str)
            .expect("Failed to add application_response template");

        let payroll_str = include_str!("../templates/payroll_template.xml");
        env.add_template("payroll", payroll_str)
            .expect("Failed to add payroll template");

        let support_doc_str = include_str!("../templates/support_document_template.xml");
        env.add_template("support_document", support_doc_str)
            .expect("Failed to add support_document template");

        Self { env }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn render_invoice(
        &self,
        customization_id: &str,
        prefix: &str,
        number: i32,
        cufe: &str,
        issue_date: &str,
        issue_time: &str,
        invoice_type_code: &str,
        supplier_nit: &str,
        supplier_name: &str,
        supplier_id_type: &str,
        customer_id: &str,
        customer_name: &str,
        customer_id_type: &str,
        net_amount: f64,
        tax_amount: f64,
        total_amount: f64,
    ) -> Result<String, UblError> {
        let tmpl = self.env.get_template("invoice")?;

        let net_amount_str = format!("{:.2}", net_amount);
        let tax_amount_str = format!("{:.2}", tax_amount);
        let total_amount_str = format!("{:.2}", total_amount);

        let rendered = tmpl.render(context! {
            CustomizationID => customization_id,
            Prefix => prefix,
            Number => number,
            CUFE => cufe,
            IssueDate => issue_date,
            IssueTime => issue_time,
            InvoiceTypeCode => invoice_type_code,
            SupplierNIT => supplier_nit,
            SupplierName => supplier_name,
            SupplierIDType => supplier_id_type,
            CustomerID => customer_id,
            CustomerName => customer_name,
            CustomerIDType => customer_id_type,
            NetAmount => net_amount_str,
            TaxAmount => tax_amount_str,
            TotalAmount => total_amount_str,
        })?;

        Ok(rendered)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn render_attached_document(
        &self,
        unique_id: &str,
        issue_date: &str,
        issue_time: &str,
        parent_document_id: &str,
        parent_document_uuid: &str,
        parent_document_issue_date: &str,
        supplier_nit: &str,
        supplier_name: &str,
        supplier_id_type: &str,
        customer_id: &str,
        customer_name: &str,
        customer_id_type: &str,
        signed_invoice_xml: &str,
        dian_response_xml: &str,
    ) -> Result<String, UblError> {
        let tmpl = self.env.get_template("attached_document")?;

        let rendered = tmpl.render(context! {
            UniqueID => unique_id,
            IssueDate => issue_date,
            IssueTime => issue_time,
            ParentDocumentID => parent_document_id,
            ParentDocumentUUID => parent_document_uuid,
            ParentDocumentIssueDate => parent_document_issue_date,
            SupplierNIT => supplier_nit,
            SupplierName => supplier_name,
            SupplierIDType => supplier_id_type,
            CustomerID => customer_id,
            CustomerName => customer_name,
            CustomerIDType => customer_id_type,
            SignedInvoiceXML => signed_invoice_xml,
            DianResponseXML => dian_response_xml,
        })?;

        Ok(rendered)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn render_application_response(
        &self,
        id: &str,
        cude: &str,
        issue_date: &str,
        issue_time: &str,
        sender_nit: &str,
        sender_name: &str,
        sender_id_type: &str,
        receiver_nit: &str,
        receiver_name: &str,
        receiver_id_type: &str,
        event_code: &str,
        event_description: &str,
        parent_document_id: &str,
        parent_document_uuid: &str,
    ) -> Result<String, UblError> {
        let tmpl = self.env.get_template("application_response")?;

        let rendered = tmpl.render(context! {
            ID => id,
            CUDE => cude,
            IssueDate => issue_date,
            IssueTime => issue_time,
            SenderNIT => sender_nit,
            SenderName => sender_name,
            SenderIDType => sender_id_type,
            ReceiverNIT => receiver_nit,
            ReceiverName => receiver_name,
            ReceiverIDType => receiver_id_type,
            EventCode => event_code,
            EventDescription => event_description,
            ParentDocumentID => parent_document_id,
            ParentDocumentUUID => parent_document_uuid,
        })?;

        Ok(rendered)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn render_payroll(
        &self,
        prefix: &str,
        number: i32,
        cune: &str,
        issue_date: &str,
        issue_time: &str,
        employer_nit: &str,
        employer_name: &str,
        employee_id: &str,
        employee_name: &str,
        devengado: f64,
        deducido: f64,
        total: f64,
    ) -> Result<String, UblError> {
        let tmpl = self.env.get_template("payroll")?;

        let devengado_str = format!("{:.2}", devengado);
        let deducido_str = format!("{:.2}", deducido);
        let total_str = format!("{:.2}", total);

        let rendered = tmpl.render(context! {
            Prefix => prefix,
            Number => number,
            CUNE => cune,
            IssueDate => issue_date,
            IssueTime => issue_time,
            EmployerNIT => employer_nit,
            EmployerName => employer_name,
            EmployeeID => employee_id,
            EmployeeName => employee_name,
            Devengado => devengado_str,
            Deducido => deducido_str,
            Total => total_str,
        })?;

        Ok(rendered)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn render_support_document(
        &self,
        prefix: &str,
        number: i32,
        cuds: &str,
        issue_date: &str,
        issue_time: &str,
        seller_nit: &str,
        seller_name: &str,
        buyer_nit: &str,
        buyer_name: &str,
        net_amount: f64,
        tax_amount: f64,
        total_amount: f64,
    ) -> Result<String, UblError> {
        let tmpl = self.env.get_template("support_document")?;

        let net_amount_str = format!("{:.2}", net_amount);
        let tax_amount_str = format!("{:.2}", tax_amount);
        let total_amount_str = format!("{:.2}", total_amount);

        let rendered = tmpl.render(context! {
            Prefix => prefix,
            Number => number,
            CUDS => cuds,
            IssueDate => issue_date,
            IssueTime => issue_time,
            SellerNIT => seller_nit,
            SellerName => seller_name,
            BuyerNIT => buyer_nit,
            BuyerName => buyer_name,
            NetAmount => net_amount_str,
            TaxAmount => tax_amount_str,
            TotalAmount => total_amount_str,
        })?;

        Ok(rendered)
    }
}
