use anyhow::Context;
use quick_xml::Writer;
use quick_xml::events::{BytesDecl, BytesEnd, BytesStart, BytesText, Event};
use std::fmt::Write as _;
use std::io::Cursor;

use crate::models::{Invoice, Money, Party};

pub fn build_invoice_xml(invoice: &Invoice) -> anyhow::Result<String> {
    let mut writer = Writer::new_with_indent(Cursor::new(Vec::new()), b' ', 2);
    writer.write_event(Event::Decl(BytesDecl::new("1.0", Some("UTF-8"), None)))?;

    let mut root = BytesStart::new("invoice");
    root.push_attribute(("xmlns:xsi", "http://www.w3.org/2001/XMLSchema-instance"));
    root.push_attribute(("xmlns", "https://hpkns.uk/accounting"));
    root.push_attribute(("defaultCurrency", invoice.currency.as_str()));
    writer.write_event(Event::Start(root))?;

    write_text_element(
        &mut writer,
        "paymentDueBy",
        &invoice.payment_due_by.to_string(),
    )?;

    writer.write_event(Event::Start(BytesStart::new("parties")))?;
    write_party(&mut writer, "us", &invoice.supplier)?;
    write_party(&mut writer, "them", &invoice.customer)?;
    writer.write_event(Event::End(BytesEnd::new("parties")))?;

    writer.write_event(Event::Start(BytesStart::new("items")))?;
    for item in &invoice.items {
        writer.write_event(Event::Start(BytesStart::new("item")))?;
        write_text_element(&mut writer, "date", &item.date.to_string())?;
        write_text_element(&mut writer, "summary", &item.summary)?;
        if let Some(details) = &item.details {
            write_text_element(&mut writer, "details", details)?;
        }
        if let Some(link) = &item.link {
            write_text_element(&mut writer, "link", link)?;
        }
        for tag in &item.tags {
            write_text_element(&mut writer, "tag", tag)?;
        }
        write_money_element(&mut writer, "debit", &item.debit, &invoice.currency)?;
        if let Some(credit) = &item.credit {
            write_money_element(&mut writer, "credit", credit, &invoice.currency)?;
        }
        writer.write_event(Event::End(BytesEnd::new("item")))?;
    }
    writer.write_event(Event::End(BytesEnd::new("items")))?;

    if let Some(notes) = &invoice.notes {
        write_text_element(&mut writer, "notes", notes)?;
    }

    writer.write_event(Event::End(BytesEnd::new("invoice")))?;
    let bytes = writer.into_inner().into_inner();
    String::from_utf8(bytes).context("generated XML was not UTF-8")
}

pub fn find_xml_value(body: &str, element: &str) -> Option<String> {
    let start_tag = format!("<{element}>");
    let end_tag = format!("</{element}>");
    let start = body.find(&start_tag)? + start_tag.len();
    let end = body[start..].find(&end_tag)? + start;
    Some(body[start..end].trim().to_string())
}

fn write_text_element(
    writer: &mut Writer<Cursor<Vec<u8>>>,
    name: &str,
    value: &str,
) -> anyhow::Result<()> {
    writer.write_event(Event::Start(BytesStart::new(name)))?;
    writer.write_event(Event::Text(BytesText::new(value)))?;
    writer.write_event(Event::End(BytesEnd::new(name)))?;
    Ok(())
}

fn write_party(
    writer: &mut Writer<Cursor<Vec<u8>>>,
    node_name: &str,
    party: &Party,
) -> anyhow::Result<()> {
    writer.write_event(Event::Start(BytesStart::new(node_name)))?;
    write_text_element(writer, "name", &party.name)?;
    write_text_element(writer, "address", &party.address)?;
    write_text_element(writer, "email", &party.email)?;
    writer.write_event(Event::End(BytesEnd::new(node_name)))?;
    Ok(())
}

fn write_money_element(
    writer: &mut Writer<Cursor<Vec<u8>>>,
    name: &str,
    money: &Money,
    default_currency: &str,
) -> anyhow::Result<()> {
    let mut element = BytesStart::new(name);
    if money.currency != default_currency {
        element.push_attribute(("currency", money.currency.as_str()));
    }
    writer.write_event(Event::Start(element))?;
    writer.write_event(Event::Text(BytesText::new(&format_money(money.value))))?;
    writer.write_event(Event::End(BytesEnd::new(name)))?;
    Ok(())
}

fn format_money(value: f64) -> String {
    let mut out = String::new();
    let rounded = (value * 100.0).round() / 100.0;
    let _ = write!(&mut out, "{rounded:.2}");
    out
}
