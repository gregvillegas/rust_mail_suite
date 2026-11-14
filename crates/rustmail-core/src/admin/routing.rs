//! Aliases and groups (distribution lists).

use chrono::{DateTime, Utc};
use serde_json::json;
use uuid::Uuid;

use super::{like_contains, Admin, AdminService};
use crate::domain::validation::{normalize_email, split_email};
use crate::domain::AuditOutcome;
use crate::error::{Error, Result};

/// Largest page of aliases.
pub const MAX_PAGE_SIZE: i64 = 200;

/// An alias.
#[derive(Debug, Clone, PartialEq)]
pub struct AliasRow {
    /// Id.
    pub id: Uuid,
    /// Domain id.
    pub domain_id: Uuid,
    /// Domain name.
    pub domain: String,
    /// The address that is delivered elsewhere.
    pub address: String,
    /// Where it is delivered.
    pub destination: String,
    /// Whether it is in effect.
    pub enabled: bool,
    /// When it was created.
    pub created_at: DateTime<Utc>,
}

/// A page of aliases.
#[derive(Debug, Clone, PartialEq)]
pub struct AliasPage {
    /// The rows.
    pub rows: Vec<AliasRow>,
    /// Matches in total.
    pub total: i64,
}

/// A group in a list.
#[derive(Debug, Clone, PartialEq)]
pub struct GroupRow {
    /// Id.
    pub id: Uuid,
    /// Domain id.
    pub domain_id: Uuid,
    /// Domain name.
    pub domain: String,
    /// Name.
    pub name: String,
    /// The list address, if any.
    pub address: Option<String>,
    /// Description.
    pub description: String,
    /// Number of members.
    pub members: i64,
}

/// A member of a group.
#[derive(Debug, Clone, PartialEq)]
pub struct Member {
    /// User id.
    pub user_id: Uuid,
    /// Address.
    pub email: String,
    /// Display name.
    pub display_name: String,
}

/// A group with its members.
#[derive(Debug, Clone, PartialEq)]
pub struct GroupDetail {
    /// The group.
    pub group: GroupRow,
    /// Its members.
    pub members: Vec<Member>,
}

impl AdminService {
    /// Lists aliases the administrator may see.
    ///
    /// # Errors
    /// [`Error::Forbidden`] for a domain outside the scope.
    pub async fn list_aliases(
        &self,
        admin: &Admin,
        domain_id: Option<Uuid>,
        search: Option<&str>,
        limit: i64,
        offset: i64,
    ) -> Result<AliasPage> {
        let domain = match admin.scope.domain_filter() {
            Some(own) => {
                if domain_id.is_some_and(|d| d != own) {
                    return Err(Error::Forbidden("that belongs to another domain".into()));
                }
                Some(own)
            }
            None => domain_id,
        };
        let pattern = search.and_then(like_contains);
        let (limit, offset) = (limit.clamp(1, MAX_PAGE_SIZE), offset.max(0));
        let total = sqlx::query_scalar!(
            r#"SELECT count(*) AS "n!" FROM aliases a
               WHERE ($1::uuid IS NULL OR a.domain_id = $1)
                 AND ($2::text IS NULL OR a.address ILIKE $2 OR a.destination ILIKE $2)"#,
            domain,
            pattern
        )
        .fetch_one(self.pool())
        .await?;
        let rows = sqlx::query!(
            r#"SELECT a.id, a.domain_id, d.name AS domain, a.address, a.destination, a.enabled, a.created_at
               FROM aliases a JOIN domains d ON d.id = a.domain_id
               WHERE ($1::uuid IS NULL OR a.domain_id = $1)
                 AND ($2::text IS NULL OR a.address ILIKE $2 OR a.destination ILIKE $2)
               ORDER BY a.address, a.destination LIMIT $3 OFFSET $4"#,
            domain,
            pattern,
            limit,
            offset
        )
        .fetch_all(self.pool())
        .await?
        .into_iter()
        .map(|r| AliasRow {
            id: r.id,
            domain_id: r.domain_id,
            domain: r.domain,
            address: r.address,
            destination: r.destination,
            enabled: r.enabled,
            created_at: r.created_at,
        })
        .collect();
        Ok(AliasPage { rows, total })
    }

    /// Creates an alias. `address` may be a bare local part (`info`) or a full
    /// address in the domain.
    ///
    /// # Errors
    /// [`Error::Forbidden`], [`Error::Validation`], [`Error::Conflict`] if the
    /// address is already a mailbox or group, or the alias exists.
    pub async fn create_alias(
        &self,
        admin: &Admin,
        domain_id: Uuid,
        address: &str,
        destination: &str,
    ) -> Result<AliasRow> {
        let domain = self.scoped_domain(admin, domain_id).await?;
        let address = if address.contains('@') {
            normalize_email(address)?
        } else {
            normalize_email(&format!("{}@{}", address.trim(), domain.name))?
        };
        let (_, address_domain) = split_email(&address)?;
        if address_domain != domain.name {
            return Err(Error::Validation(format!(
                "the address must be in {}",
                domain.name
            )));
        }
        let destination = normalize_email(destination)?;
        if destination == address {
            return Err(Error::Validation("an alias cannot point to itself".into()));
        }
        // A mailbox, alias-source or group with this address would be shadowed by, or
        // would shadow, the alias; Postfix consults aliases first. Only another alias
        // for the same address (a second destination) is fine.
        let clash = sqlx::query_scalar!(
            r#"SELECT (EXISTS (SELECT 1 FROM users WHERE email = $1)
                    OR EXISTS (SELECT 1 FROM groups WHERE address = $1)) AS "clash!""#,
            address
        )
        .fetch_one(self.pool())
        .await?;
        if clash {
            return Err(Error::Conflict(format!(
                "{address} is already a mailbox or a group"
            )));
        }
        let saved = sqlx::query!(
            r#"INSERT INTO aliases (domain_id, address, destination) VALUES ($1, $2, $3)
               RETURNING id, created_at"#,
            domain_id,
            address,
            destination
        )
        .fetch_one(self.pool())
        .await
        .map_err(Error::from)?;
        self.audit(
            admin,
            "alias.create",
            Some(("alias", saved.id.to_string())),
            Some(domain_id),
            AuditOutcome::Success,
            json!({"address": address, "destination": destination}),
        )
        .await;
        Ok(AliasRow {
            id: saved.id,
            domain_id,
            domain: domain.name,
            address,
            destination,
            enabled: true,
            created_at: saved.created_at,
        })
    }

    async fn alias_in_scope(&self, admin: &Admin, id: Uuid) -> Result<(Uuid, String, String)> {
        let row = sqlx::query!(
            "SELECT domain_id, address, destination FROM aliases WHERE id = $1",
            id
        )
        .fetch_optional(self.pool())
        .await?
        .ok_or(Error::NotFound { entity: "alias" })?;
        admin.require_domain(row.domain_id)?;
        Ok((row.domain_id, row.address, row.destination))
    }

    /// Turns an alias on or off.
    ///
    /// # Errors
    /// [`Error::Forbidden`], [`Error::NotFound`].
    pub async fn set_alias_enabled(&self, admin: &Admin, id: Uuid, enabled: bool) -> Result<()> {
        let (domain_id, address, destination) = self.alias_in_scope(admin, id).await?;
        sqlx::query!("UPDATE aliases SET enabled = $2 WHERE id = $1", id, enabled)
            .execute(self.pool())
            .await?;
        self.audit(
            admin,
            "alias.toggle",
            Some(("alias", id.to_string())),
            Some(domain_id),
            AuditOutcome::Success,
            json!({"address": address, "destination": destination, "enabled": enabled}),
        )
        .await;
        Ok(())
    }

    /// Deletes an alias.
    ///
    /// # Errors
    /// [`Error::Forbidden`], [`Error::NotFound`].
    pub async fn delete_alias(&self, admin: &Admin, id: Uuid) -> Result<()> {
        let (domain_id, address, destination) = self.alias_in_scope(admin, id).await?;
        sqlx::query!("DELETE FROM aliases WHERE id = $1", id)
            .execute(self.pool())
            .await?;
        self.audit(
            admin,
            "alias.delete",
            Some(("alias", id.to_string())),
            Some(domain_id),
            AuditOutcome::Success,
            json!({"address": address, "destination": destination}),
        )
        .await;
        Ok(())
    }

    /// Lists groups the administrator may see.
    ///
    /// # Errors
    /// [`Error::Forbidden`] for a domain outside the scope.
    pub async fn list_groups(
        &self,
        admin: &Admin,
        domain_id: Option<Uuid>,
    ) -> Result<Vec<GroupRow>> {
        let domain = match admin.scope.domain_filter() {
            Some(own) => {
                if domain_id.is_some_and(|d| d != own) {
                    return Err(Error::Forbidden("that belongs to another domain".into()));
                }
                Some(own)
            }
            None => domain_id,
        };
        Ok(sqlx::query!(
            r#"SELECT g.id, g.domain_id, d.name AS domain, g.name, g.address, g.description,
                      (SELECT count(*) FROM group_members gm WHERE gm.group_id = g.id) AS "members!"
               FROM groups g JOIN domains d ON d.id = g.domain_id
               WHERE ($1::uuid IS NULL OR g.domain_id = $1)
               ORDER BY d.name, g.name"#,
            domain
        )
        .fetch_all(self.pool())
        .await?
        .into_iter()
        .map(|r| GroupRow {
            id: r.id,
            domain_id: r.domain_id,
            domain: r.domain,
            name: r.name,
            address: r.address,
            description: r.description,
            members: r.members,
        })
        .collect())
    }

    /// A group and its members.
    ///
    /// # Errors
    /// [`Error::Forbidden`], [`Error::NotFound`].
    pub async fn get_group(&self, admin: &Admin, id: Uuid) -> Result<GroupDetail> {
        let domain = sqlx::query_scalar!("SELECT domain_id FROM groups WHERE id = $1", id)
            .fetch_optional(self.pool())
            .await?
            .ok_or(Error::NotFound { entity: "group" })?;
        admin.require_domain(domain)?;
        let group = self
            .list_groups(admin, Some(domain))
            .await?
            .into_iter()
            .find(|g| g.id == id)
            .ok_or(Error::NotFound { entity: "group" })?;
        let members = sqlx::query!(
            "SELECT u.id, u.email, u.display_name FROM group_members gm JOIN users u ON u.id = gm.user_id
             WHERE gm.group_id = $1 ORDER BY u.email",
            id
        )
        .fetch_all(self.pool())
        .await?
        .into_iter()
        .map(|r| Member { user_id: r.id, email: r.email, display_name: r.display_name })
        .collect();
        Ok(GroupDetail { group, members })
    }

    /// Creates a group, optionally with a list address that delivers to all members.
    ///
    /// # Errors
    /// [`Error::Forbidden`], [`Error::Validation`], [`Error::Conflict`].
    pub async fn create_group(
        &self,
        admin: &Admin,
        domain_id: Uuid,
        name: &str,
        address: Option<&str>,
        description: &str,
    ) -> Result<Uuid> {
        let domain = self.scoped_domain(admin, domain_id).await?;
        let name = name.trim();
        if name.is_empty() || name.chars().count() > 128 || name.chars().any(char::is_control) {
            return Err(Error::Validation(
                "the group name must be 1 to 128 characters".into(),
            ));
        }
        let description = description.trim();
        if description.chars().count() > 500
            || description.chars().any(|c| c.is_control() && c != '\n')
        {
            return Err(Error::Validation("the description is too long".into()));
        }
        let address = match address.map(str::trim).filter(|a| !a.is_empty()) {
            None => None,
            Some(a) => {
                let full = if a.contains('@') {
                    normalize_email(a)?
                } else {
                    normalize_email(&format!("{a}@{}", domain.name))?
                };
                if split_email(&full)?.1 != domain.name {
                    return Err(Error::Validation(format!(
                        "the address must be in {}",
                        domain.name
                    )));
                }
                if self.address_taken(&full).await? {
                    return Err(Error::Conflict(format!("{full} is already in use")));
                }
                Some(full)
            }
        };
        let id = sqlx::query_scalar!(
            "INSERT INTO groups (domain_id, name, address, description) VALUES ($1, $2, $3, $4) RETURNING id",
            domain_id,
            name,
            address,
            description
        )
        .fetch_one(self.pool())
        .await
        .map_err(Error::from)?;
        self.audit(
            admin,
            "group.create",
            Some(("group", id.to_string())),
            Some(domain_id),
            AuditOutcome::Success,
            json!({"name": name, "address": address}),
        )
        .await;
        Ok(id)
    }

    /// Deletes a group.
    ///
    /// # Errors
    /// [`Error::Forbidden`], [`Error::NotFound`].
    pub async fn delete_group(&self, admin: &Admin, id: Uuid) -> Result<()> {
        let detail = self.get_group(admin, id).await?;
        sqlx::query!("DELETE FROM groups WHERE id = $1", id)
            .execute(self.pool())
            .await?;
        self.audit(
            admin,
            "group.delete",
            Some(("group", id.to_string())),
            Some(detail.group.domain_id),
            AuditOutcome::Success,
            json!({"name": detail.group.name, "members": detail.members.len()}),
        )
        .await;
        Ok(())
    }

    /// Adds a user (of the same domain) to a group.
    ///
    /// # Errors
    /// [`Error::Forbidden`], [`Error::Validation`] if there is no such user in the
    /// group's domain.
    pub async fn add_group_member(&self, admin: &Admin, group_id: Uuid, email: &str) -> Result<()> {
        let detail = self.get_group(admin, group_id).await?;
        let email = normalize_email(email)?;
        let user = sqlx::query!(
            "SELECT id FROM users WHERE email = $1 AND domain_id = $2",
            email,
            detail.group.domain_id
        )
        .fetch_optional(self.pool())
        .await?
        .ok_or_else(|| {
            Error::Validation(format!("{email} is not a user of {}", detail.group.domain))
        })?;
        sqlx::query!(
            "INSERT INTO group_members (group_id, user_id) VALUES ($1, $2) ON CONFLICT DO NOTHING",
            group_id,
            user.id
        )
        .execute(self.pool())
        .await?;
        self.audit(
            admin,
            "group.member_add",
            Some(("group", group_id.to_string())),
            Some(detail.group.domain_id),
            AuditOutcome::Success,
            json!({"group": detail.group.name, "member": email}),
        )
        .await;
        Ok(())
    }

    /// Removes a member from a group.
    ///
    /// # Errors
    /// [`Error::Forbidden`], [`Error::NotFound`].
    pub async fn remove_group_member(
        &self,
        admin: &Admin,
        group_id: Uuid,
        user_id: Uuid,
    ) -> Result<()> {
        let detail = self.get_group(admin, group_id).await?;
        let member = detail
            .members
            .iter()
            .find(|m| m.user_id == user_id)
            .ok_or(Error::NotFound { entity: "member" })?;
        sqlx::query!(
            "DELETE FROM group_members WHERE group_id = $1 AND user_id = $2",
            group_id,
            user_id
        )
        .execute(self.pool())
        .await?;
        self.audit(
            admin,
            "group.member_remove",
            Some(("group", group_id.to_string())),
            Some(detail.group.domain_id),
            AuditOutcome::Success,
            json!({"group": detail.group.name, "member": member.email}),
        )
        .await;
        Ok(())
    }
}
