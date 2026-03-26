//! EWS SOAP/XML request builders and response parsers.

use quick_xml::Reader;
use quick_xml::events::Event;

const SOAP_NS: &str = "http://schemas.xmlsoap.org/soap/envelope/";
const TYPES_NS: &str = "http://schemas.microsoft.com/exchange/services/2006/types";
const MESSAGES_NS: &str = "http://schemas.microsoft.com/exchange/services/2006/messages";

/// Build a FindItem SOAP request using CalendarView (expands recurrences).
pub fn build_find_item_calendar_view(
    start_date: &str,
    end_date: &str,
    max_entries: u32,
    folder_id: Option<&str>,
) -> String {
    let folder = folder_id_xml(folder_id);
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<soap:Envelope xmlns:soap="{SOAP_NS}"
               xmlns:t="{TYPES_NS}"
               xmlns:m="{MESSAGES_NS}">
  <soap:Body>
    <m:FindItem Traversal="Shallow">
      <m:ItemShape>
        <t:BaseShape>Default</t:BaseShape>
        <t:AdditionalProperties>
          <t:FieldURI FieldURI="calendar:Start"/>
          <t:FieldURI FieldURI="calendar:End"/>
          <t:FieldURI FieldURI="calendar:Location"/>
          <t:FieldURI FieldURI="calendar:Organizer"/>
          <t:FieldURI FieldURI="calendar:IsRecurring"/>
          <t:FieldURI FieldURI="calendar:IsAllDayEvent"/>
          <t:FieldURI FieldURI="item:Subject"/>
          <t:FieldURI FieldURI="item:DateTimeReceived"/>
          <t:FieldURI FieldURI="item:ItemId"/>
        </t:AdditionalProperties>
      </m:ItemShape>
      <m:CalendarView StartDate="{start_date}" EndDate="{end_date}" MaxEntriesReturned="{max_entries}"/>
      <m:ParentFolderIds>
        {folder}
      </m:ParentFolderIds>
    </m:FindItem>
  </soap:Body>
</soap:Envelope>"#
    )
}

/// Build a FindItem SOAP request with text search restriction.
pub fn build_find_item_search(
    query: &str,
    max_entries: u32,
    folder_id: Option<&str>,
) -> String {
    let folder = folder_id_xml(folder_id);
    let escaped_query = quick_xml::escape::escape(query);
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<soap:Envelope xmlns:soap="{SOAP_NS}"
               xmlns:t="{TYPES_NS}"
               xmlns:m="{MESSAGES_NS}">
  <soap:Body>
    <m:FindItem Traversal="Shallow">
      <m:ItemShape>
        <t:BaseShape>Default</t:BaseShape>
        <t:AdditionalProperties>
          <t:FieldURI FieldURI="calendar:Start"/>
          <t:FieldURI FieldURI="calendar:End"/>
          <t:FieldURI FieldURI="calendar:Location"/>
          <t:FieldURI FieldURI="calendar:Organizer"/>
          <t:FieldURI FieldURI="calendar:IsRecurring"/>
          <t:FieldURI FieldURI="calendar:IsAllDayEvent"/>
          <t:FieldURI FieldURI="item:Subject"/>
          <t:FieldURI FieldURI="item:DateTimeReceived"/>
          <t:FieldURI FieldURI="item:ItemId"/>
        </t:AdditionalProperties>
      </m:ItemShape>
      <m:IndexedPageItemView MaxEntriesReturned="{max_entries}" Offset="0" BasePoint="Beginning"/>
      <m:Restriction>
        <t:Or>
          <t:Contains ContainmentMode="Substring" ContainmentComparison="IgnoreCase">
            <t:FieldURI FieldURI="item:Subject"/>
            <t:Constant Value="{escaped_query}"/>
          </t:Contains>
          <t:Contains ContainmentMode="Substring" ContainmentComparison="IgnoreCase">
            <t:FieldURI FieldURI="item:Body"/>
            <t:Constant Value="{escaped_query}"/>
          </t:Contains>
        </t:Or>
      </m:Restriction>
      <m:SortOrder>
        <t:FieldOrder Order="Descending">
          <t:FieldURI FieldURI="calendar:Start"/>
        </t:FieldOrder>
      </m:SortOrder>
      <m:ParentFolderIds>
        {folder}
      </m:ParentFolderIds>
    </m:FindItem>
  </soap:Body>
</soap:Envelope>"#
    )
}

/// Build a GetItem SOAP request for full calendar event details.
pub fn build_get_item(item_id: &str) -> String {
    let escaped_id = quick_xml::escape::escape(item_id);
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<soap:Envelope xmlns:soap="{SOAP_NS}"
               xmlns:t="{TYPES_NS}"
               xmlns:m="{MESSAGES_NS}">
  <soap:Body>
    <m:GetItem>
      <m:ItemShape>
        <t:BaseShape>AllProperties</t:BaseShape>
        <t:BodyType>Text</t:BodyType>
      </m:ItemShape>
      <m:ItemIds>
        <t:ItemId Id="{escaped_id}"/>
      </m:ItemIds>
    </m:GetItem>
  </soap:Body>
</soap:Envelope>"#
    )
}

/// Build a FindFolder SOAP request for listing calendar folders.
#[allow(dead_code)]
pub fn build_find_calendar_folders() -> String {
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<soap:Envelope xmlns:soap="{SOAP_NS}"
               xmlns:t="{TYPES_NS}"
               xmlns:m="{MESSAGES_NS}">
  <soap:Body>
    <m:FindFolder Traversal="Deep">
      <m:FolderShape>
        <t:BaseShape>Default</t:BaseShape>
      </m:FolderShape>
      <m:ParentFolderIds>
        <t:DistinguishedFolderId Id="calendar"/>
      </m:ParentFolderIds>
    </m:FindFolder>
  </soap:Body>
</soap:Envelope>"#
    )
}

fn folder_id_xml(folder_id: Option<&str>) -> String {
    match folder_id {
        Some(id) if !id.is_empty() && id != "calendar" && id != "Calendar" => {
            let escaped = quick_xml::escape::escape(id);
            format!(r#"<t:FolderId Id="{escaped}"/>"#)
        }
        _ => r#"<t:DistinguishedFolderId Id="calendar"/>"#.to_string(),
    }
}

// ──────────────────── Response Parsers ────────────────────

/// Parsed calendar item from EWS FindItem response.
#[derive(Debug, Clone, Default)]
pub struct EwsCalendarItem {
    pub item_id: String,
    pub change_key: String,
    pub subject: String,
    pub start: String,
    pub end: String,
    pub location: String,
    pub organizer: String,
    pub is_recurring: bool,
    pub is_all_day: bool,
}

/// Parsed full calendar item from EWS GetItem response.
#[derive(Debug, Clone, Default)]
pub struct EwsCalendarItemDetail {
    pub item_id: String,
    pub change_key: String,
    pub subject: String,
    pub start: String,
    pub end: String,
    pub location: String,
    pub organizer: String,
    pub is_recurring: bool,
    pub is_all_day: bool,
    pub body: String,
    pub attendees: Vec<String>,
    pub categories: Vec<String>,
    pub uid: String,
    pub status: String,
    pub importance: String,
    pub sensitivity: String,
}

/// Parse a FindItem response to extract calendar items.
pub fn parse_find_item_response(xml: &str) -> anyhow::Result<Vec<EwsCalendarItem>> {
    // Check for SOAP fault first
    check_soap_fault(xml)?;

    let mut reader = Reader::from_str(xml);
    let mut items = Vec::new();
    let mut current_item: Option<EwsCalendarItem> = None;
    let mut current_tag = String::new();
    let mut in_calendar_item = false;
    let mut in_organizer = false;
    let mut in_mailbox = false;
    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) | Ok(Event::Empty(ref e)) => {
                let name = e.name();
                let local = local_name(name.as_ref());
                match local.as_str() {
                    "CalendarItem" => {
                        in_calendar_item = true;
                        current_item = Some(EwsCalendarItem::default());
                    }
                    "ItemId" if in_calendar_item => {
                        if let Some(ref mut item) = current_item {
                            for attr in e.attributes().flatten() {
                                match attr.key.as_ref() {
                                    b"Id" => {
                                        item.item_id =
                                            String::from_utf8_lossy(&attr.value).to_string();
                                    }
                                    b"ChangeKey" => {
                                        item.change_key =
                                            String::from_utf8_lossy(&attr.value).to_string();
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                    "Organizer" if in_calendar_item => {
                        in_organizer = true;
                    }
                    "Mailbox" if in_organizer => {
                        in_mailbox = true;
                    }
                    _ if in_calendar_item => {
                        current_tag = local.to_string();
                    }
                    _ => {}
                }
            }
            Ok(Event::Text(ref e)) if in_calendar_item => {
                let text = e.unescape().unwrap_or_default().to_string();
                if let Some(ref mut item) = current_item {
                    match current_tag.as_str() {
                        "Subject" => item.subject = text,
                        "Start" => item.start = text,
                        "End" => item.end = text,
                        "Location" => item.location = text,
                        "IsRecurring" => item.is_recurring = text == "true",
                        "IsAllDayEvent" => item.is_all_day = text == "true",
                        "Name" if in_organizer && in_mailbox => item.organizer = text,
                        _ => {}
                    }
                }
                current_tag.clear();
            }
            Ok(Event::End(ref e)) => {
                let name = e.name();
                let local = local_name(name.as_ref());
                match local.as_str() {
                    "CalendarItem" => {
                        if let Some(item) = current_item.take() {
                            items.push(item);
                        }
                        in_calendar_item = false;
                    }
                    "Organizer" => in_organizer = false,
                    "Mailbox" if in_organizer => in_mailbox = false,
                    _ => {}
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => {
                anyhow::bail!("EWS XML parse error: {e}");
            }
            _ => {}
        }
        buf.clear();
    }

    Ok(items)
}

/// Parse a GetItem response for full calendar event details.
pub fn parse_get_item_response(xml: &str) -> anyhow::Result<EwsCalendarItemDetail> {
    check_soap_fault(xml)?;

    let mut reader = Reader::from_str(xml);
    let mut item = EwsCalendarItemDetail::default();
    let mut current_tag = String::new();
    let mut in_calendar_item = false;
    let mut in_organizer = false;
    let mut in_mailbox = false;
    let mut in_attendee = false;
    let mut in_required_attendees = false;
    let mut in_optional_attendees = false;
    let mut in_body = false;
    let mut attendee_name = String::new();
    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) | Ok(Event::Empty(ref e)) => {
                let name = e.name();
                let local = local_name(name.as_ref());
                match local.as_str() {
                    "CalendarItem" => {
                        in_calendar_item = true;
                    }
                    "ItemId" if in_calendar_item => {
                        for attr in e.attributes().flatten() {
                            match attr.key.as_ref() {
                                b"Id" => {
                                    item.item_id =
                                        String::from_utf8_lossy(&attr.value).to_string();
                                }
                                b"ChangeKey" => {
                                    item.change_key =
                                        String::from_utf8_lossy(&attr.value).to_string();
                                }
                                _ => {}
                            }
                        }
                    }
                    "UID" if in_calendar_item => {
                        current_tag = "UID".to_string();
                    }
                    "Organizer" if in_calendar_item => {
                        in_organizer = true;
                    }
                    "Mailbox" if in_organizer || in_attendee => {
                        in_mailbox = true;
                    }
                    "RequiredAttendees" => in_required_attendees = true,
                    "OptionalAttendees" => in_optional_attendees = true,
                    "Attendee" if in_required_attendees || in_optional_attendees => {
                        in_attendee = true;
                        attendee_name.clear();
                    }
                    "Body" if in_calendar_item => {
                        in_body = true;
                        current_tag = "Body".to_string();
                    }
                    _ if in_calendar_item => {
                        current_tag = local.to_string();
                    }
                    _ => {}
                }
            }
            Ok(Event::Text(ref e)) if in_calendar_item => {
                let text = e.unescape().unwrap_or_default().to_string();
                match current_tag.as_str() {
                    "Subject" => item.subject = text,
                    "Start" => item.start = text,
                    "End" => item.end = text,
                    "Location" => item.location = text,
                    "IsRecurring" => item.is_recurring = text == "true",
                    "IsAllDayEvent" => item.is_all_day = text == "true",
                    "Body" if in_body => item.body = text,
                    "Name" if in_organizer && in_mailbox => item.organizer = text,
                    "Name" if in_attendee && in_mailbox => attendee_name = text,
                    "EmailAddress" if in_attendee && in_mailbox => {
                        let attendee = if attendee_name.is_empty() {
                            text
                        } else {
                            format!("{attendee_name} <{text}>")
                        };
                        item.attendees.push(attendee);
                    }
                    "Importance" => item.importance = text,
                    "Sensitivity" => item.sensitivity = text,
                    "LegacyFreeBusyStatus" => item.status = text,
                    "UID" => item.uid = text,
                    "String" => {
                        // Categories contain <String> elements
                        item.categories.push(text);
                    }
                    _ => {}
                }
                current_tag.clear();
            }
            Ok(Event::End(ref e)) => {
                let name = e.name();
                let local = local_name(name.as_ref());
                match local.as_str() {
                    "CalendarItem" => in_calendar_item = false,
                    "Organizer" => {
                        in_organizer = false;
                        in_mailbox = false;
                    }
                    "Attendee" => {
                        in_attendee = false;
                        in_mailbox = false;
                    }
                    "RequiredAttendees" => in_required_attendees = false,
                    "OptionalAttendees" => in_optional_attendees = false,
                    "Body" => in_body = false,
                    "Mailbox" => in_mailbox = false,
                    _ => {}
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => {
                anyhow::bail!("EWS XML parse error: {e}");
            }
            _ => {}
        }
        buf.clear();
    }

    if item.item_id.is_empty() {
        anyhow::bail!("No CalendarItem found in GetItem response");
    }

    Ok(item)
}

/// Check for SOAP Fault in the response.
fn check_soap_fault(xml: &str) -> anyhow::Result<()> {
    let mut reader = Reader::from_str(xml);
    let mut in_fault = false;
    let mut in_faultstring = false;
    let mut in_message = false;
    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) => {
                let name = e.name();
                let local = local_name(name.as_ref());
                match local.as_str() {
                    "Fault" => in_fault = true,
                    "faultstring" if in_fault => in_faultstring = true,
                    "MessageText" => in_message = true,
                    _ => {}
                }
            }
            Ok(Event::Text(ref e)) => {
                if in_faultstring || in_message {
                    let text = e.unescape().unwrap_or_default().to_string();
                    anyhow::bail!("EWS error: {text}");
                }
            }
            Ok(Event::End(ref e)) => {
                let name = e.name();
                let local = local_name(name.as_ref());
                match local.as_str() {
                    "Fault" => in_fault = false,
                    "faultstring" => in_faultstring = false,
                    "MessageText" => in_message = false,
                    _ => {}
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
        buf.clear();
    }

    Ok(())
}

/// Extract the local name from a potentially namespaced XML tag.
/// e.g., b"t:Subject" -> "Subject", b"CalendarItem" -> "CalendarItem"
fn local_name(name: &[u8]) -> String {
    let s = std::str::from_utf8(name).unwrap_or("");
    s.rsplit_once(':')
        .map(|(_, local)| local)
        .unwrap_or(s)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_find_item_response() {
        let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/">
  <s:Body>
    <m:FindItemResponse xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages"
                        xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types">
      <m:ResponseMessages>
        <m:FindItemResponseMessage ResponseClass="Success">
          <m:RootFolder TotalItemsInView="2">
            <t:Items>
              <t:CalendarItem>
                <t:ItemId Id="AAMkAD123" ChangeKey="DwAA"/>
                <t:Subject>Team Meeting</t:Subject>
                <t:Start>2024-01-15T09:00:00Z</t:Start>
                <t:End>2024-01-15T10:00:00Z</t:End>
                <t:Location>Room A</t:Location>
                <t:Organizer>
                  <t:Mailbox>
                    <t:Name>Boss</t:Name>
                  </t:Mailbox>
                </t:Organizer>
                <t:IsRecurring>false</t:IsRecurring>
                <t:IsAllDayEvent>false</t:IsAllDayEvent>
              </t:CalendarItem>
              <t:CalendarItem>
                <t:ItemId Id="AAMkAD456" ChangeKey="DwBB"/>
                <t:Subject>All Hands</t:Subject>
                <t:Start>2024-01-16T00:00:00Z</t:Start>
                <t:End>2024-01-17T00:00:00Z</t:End>
                <t:IsRecurring>true</t:IsRecurring>
                <t:IsAllDayEvent>true</t:IsAllDayEvent>
              </t:CalendarItem>
            </t:Items>
          </m:RootFolder>
        </m:FindItemResponseMessage>
      </m:ResponseMessages>
    </m:FindItemResponse>
  </s:Body>
</s:Envelope>"#;

        let items = parse_find_item_response(xml).unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].item_id, "AAMkAD123");
        assert_eq!(items[0].subject, "Team Meeting");
        assert_eq!(items[0].start, "2024-01-15T09:00:00Z");
        assert_eq!(items[0].end, "2024-01-15T10:00:00Z");
        assert_eq!(items[0].location, "Room A");
        assert_eq!(items[0].organizer, "Boss");
        assert!(!items[0].is_recurring);
        assert!(!items[0].is_all_day);

        assert_eq!(items[1].item_id, "AAMkAD456");
        assert_eq!(items[1].subject, "All Hands");
        assert!(items[1].is_recurring);
        assert!(items[1].is_all_day);
    }

    #[test]
    fn test_parse_get_item_response() {
        let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/">
  <s:Body>
    <m:GetItemResponse xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages"
                       xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types">
      <m:ResponseMessages>
        <m:GetItemResponseMessage ResponseClass="Success">
          <m:Items>
            <t:CalendarItem>
              <t:ItemId Id="AAMkAD123" ChangeKey="DwAA"/>
              <t:Subject>Team Meeting</t:Subject>
              <t:Body BodyType="Text">Weekly sync to discuss progress.</t:Body>
              <t:Start>2024-01-15T09:00:00Z</t:Start>
              <t:End>2024-01-15T10:00:00Z</t:End>
              <t:Location>Conference Room A</t:Location>
              <t:Organizer>
                <t:Mailbox>
                  <t:Name>Boss</t:Name>
                  <t:EmailAddress>boss@example.com</t:EmailAddress>
                </t:Mailbox>
              </t:Organizer>
              <t:RequiredAttendees>
                <t:Attendee>
                  <t:Mailbox>
                    <t:Name>User One</t:Name>
                    <t:EmailAddress>user1@example.com</t:EmailAddress>
                  </t:Mailbox>
                </t:Attendee>
                <t:Attendee>
                  <t:Mailbox>
                    <t:Name>User Two</t:Name>
                    <t:EmailAddress>user2@example.com</t:EmailAddress>
                  </t:Mailbox>
                </t:Attendee>
              </t:RequiredAttendees>
              <t:IsRecurring>false</t:IsRecurring>
              <t:IsAllDayEvent>false</t:IsAllDayEvent>
              <t:UID>040000008200E00074C5B7101A82E123</t:UID>
              <t:LegacyFreeBusyStatus>Busy</t:LegacyFreeBusyStatus>
              <t:Categories>
                <t:String>Work</t:String>
              </t:Categories>
            </t:CalendarItem>
          </m:Items>
        </m:GetItemResponseMessage>
      </m:ResponseMessages>
    </m:GetItemResponse>
  </s:Body>
</s:Envelope>"#;

        let detail = parse_get_item_response(xml).unwrap();
        assert_eq!(detail.item_id, "AAMkAD123");
        assert_eq!(detail.subject, "Team Meeting");
        assert_eq!(detail.body, "Weekly sync to discuss progress.");
        assert_eq!(detail.start, "2024-01-15T09:00:00Z");
        assert_eq!(detail.end, "2024-01-15T10:00:00Z");
        assert_eq!(detail.location, "Conference Room A");
        assert_eq!(detail.organizer, "Boss");
        assert_eq!(detail.attendees.len(), 2);
        assert_eq!(detail.attendees[0], "User One <user1@example.com>");
        assert_eq!(detail.attendees[1], "User Two <user2@example.com>");
        assert_eq!(detail.uid, "040000008200E00074C5B7101A82E123");
        assert_eq!(detail.status, "Busy");
        assert_eq!(detail.categories, vec!["Work"]);
        assert!(!detail.is_recurring);
    }

    #[test]
    fn test_parse_soap_fault() {
        let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/">
  <s:Body>
    <s:Fault>
      <faultcode>s:Client</faultcode>
      <faultstring>The request failed schema validation.</faultstring>
    </s:Fault>
  </s:Body>
</s:Envelope>"#;

        let result = parse_find_item_response(xml);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("schema validation"));
    }
}
