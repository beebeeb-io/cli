//! Keep an explicit vault root distinct from a failed destination lookup.
use uuid::Uuid;

pub(super) enum DestinationParent<'a> {
    Root,
    Folder(Option<&'a str>),
    Missing,
    NotFolder,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum ParentError {
    NotFound,
    NotFolder,
    MissingId,
    InvalidId,
}

impl std::fmt::Display for ParentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::NotFound => "destination parent not found",
            Self::NotFolder => "destination parent is not a folder",
            Self::MissingId => "destination folder is missing its file ID",
            Self::InvalidId => "destination folder has an invalid UUID",
        })
    }
}

/// Outer None means unchanged; only Root may return Some(None).
pub(super) fn parent_target(changed: bool, parent: DestinationParent<'_>) -> Result<Option<Option<Uuid>>, ParentError> {
    if !changed {
        return Ok(None);
    }
    let id = match parent {
        DestinationParent::Root => None,
        DestinationParent::Folder(id) => {
            let id = id.ok_or(ParentError::MissingId)?;
            Some(id.parse().map_err(|_| ParentError::InvalidId)?)
        }
        DestinationParent::Missing => return Err(ParentError::NotFound),
        DestinationParent::NotFolder => return Err(ParentError::NotFolder),
    };
    Ok(Some(id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_folder_and_unchanged_parent_targets() {
        let id = Uuid::new_v4();
        assert_eq!(parent_target(true, DestinationParent::Root), Ok(Some(None)));
        assert_eq!(
            parent_target(true, DestinationParent::Folder(Some(&id.to_string()))),
            Ok(Some(Some(id)))
        );
        assert_eq!(parent_target(false, DestinationParent::Missing), Ok(None));
    }

    #[test]
    fn unknown_parent_target_is_error() {
        assert_eq!(
            parent_target(true, DestinationParent::Missing),
            Err(ParentError::NotFound)
        );
    }

    #[test]
    fn missing_folder_id_parent_target_is_error() {
        assert_eq!(
            parent_target(true, DestinationParent::Folder(None)),
            Err(ParentError::MissingId)
        );
    }

    #[test]
    fn invalid_folder_id_parent_target_is_error() {
        for id in ["", "not-a-uuid"] {
            assert_eq!(
                parent_target(true, DestinationParent::Folder(Some(id))),
                Err(ParentError::InvalidId)
            );
        }
    }
}
