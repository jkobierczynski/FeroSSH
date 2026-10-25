//! FIDO2 through Windows' WebAuthn API (`webauthn.dll`, Windows 10 1903
//! and later), as Windows' own OpenSSH does. Windows shows its own dialog
//! and talks to the authenticator: a USB, NFC or Bluetooth security key,
//! or Windows Hello. The data to sign goes in as the client data, which
//! Windows hashes with SHA-256 to make the client data hash OpenSSH's
//! signature format expects (see [`crate::sk`]).
//!
//! Runs on the broker thread, in the UI process; the worker never reaches
//! the authenticator.

#![allow(unsafe_code)]

use windows::Win32::Networking::WindowsWebServices::{
    WEBAUTHN_ATTESTATION_CONVEYANCE_PREFERENCE_NONE, WEBAUTHN_AUTHENTICATOR_ATTACHMENT_ANY,
    WEBAUTHN_AUTHENTICATOR_ATTACHMENT_PLATFORM, WEBAUTHN_AUTHENTICATOR_GET_ASSERTION_OPTIONS,
    WEBAUTHN_AUTHENTICATOR_GET_ASSERTION_OPTIONS_VERSION_1,
    WEBAUTHN_AUTHENTICATOR_MAKE_CREDENTIAL_OPTIONS,
    WEBAUTHN_AUTHENTICATOR_MAKE_CREDENTIAL_OPTIONS_VERSION_1, WEBAUTHN_CLIENT_DATA,
    WEBAUTHN_CLIENT_DATA_CURRENT_VERSION, WEBAUTHN_COSE_CREDENTIAL_PARAMETER,
    WEBAUTHN_COSE_CREDENTIAL_PARAMETER_CURRENT_VERSION, WEBAUTHN_COSE_CREDENTIAL_PARAMETERS,
    WEBAUTHN_CREDENTIAL, WEBAUTHN_CREDENTIAL_CURRENT_VERSION, WEBAUTHN_CREDENTIAL_TYPE_PUBLIC_KEY,
    WEBAUTHN_CREDENTIALS, WEBAUTHN_HASH_ALGORITHM_SHA_256, WEBAUTHN_RP_ENTITY_INFORMATION,
    WEBAUTHN_RP_ENTITY_INFORMATION_CURRENT_VERSION, WEBAUTHN_USER_ENTITY_INFORMATION,
    WEBAUTHN_USER_ENTITY_INFORMATION_CURRENT_VERSION,
    WEBAUTHN_USER_VERIFICATION_REQUIREMENT_DISCOURAGED,
    WEBAUTHN_USER_VERIFICATION_REQUIREMENT_REQUIRED, WebAuthNAuthenticatorGetAssertion,
    WebAuthNAuthenticatorMakeCredential, WebAuthNFreeAssertion, WebAuthNFreeCredentialAttestation,
    WebAuthNGetErrorName,
};
use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;
use windows::core::{BOOL, HSTRING, PCWSTR};

use crate::sk::{Assertion, Authenticator, EnrollRequest, SignRequest, USER_VERIFICATION};

/// How long Windows waits for a touch, as OpenSSH does.
const TIMEOUT_MS: u32 = 60_000;

/// Windows' WebAuthn API. See the module documentation.
#[derive(Debug, Clone, Copy, Default)]
pub struct WebAuthn;

fn error(e: &windows::core::Error) -> String {
    // SAFETY: returns a static string for any HRESULT.
    let name = unsafe { WebAuthNGetErrorName(e.code()).to_string() }.unwrap_or_default();
    match name.as_str() {
        // Cancelled, timed out, or no authenticator has the credential.
        "NotAllowedError" => {
            "cancelled, timed out, or this security key does not hold the key".into()
        }
        "" => e.message(),
        _ => format!("{} ({name})", e.message()),
    }
}

/// Copies `len` bytes the API returned.
///
/// # Safety
/// `ptr` is null or points to `len` readable bytes.
unsafe fn bytes(ptr: *const u8, len: u32) -> Vec<u8> {
    if ptr.is_null() {
        return Vec::new();
    }
    // SAFETY: as the caller promises.
    unsafe { std::slice::from_raw_parts(ptr, len as usize) }.to_vec()
}

fn len(b: &[u8]) -> Result<u32, String> {
    u32::try_from(b.len()).map_err(|_| "too much data for WebAuthn".to_owned())
}

fn uv(required: bool) -> u32 {
    if required {
        WEBAUTHN_USER_VERIFICATION_REQUIREMENT_REQUIRED
    } else {
        WEBAUTHN_USER_VERIFICATION_REQUIREMENT_DISCOURAGED
    }
}

impl Authenticator for WebAuthn {
    fn sign(&self, request: &SignRequest<'_>) -> Result<Assertion, String> {
        let rp = HSTRING::from(request.application);
        let mut message = request.message.to_vec();
        let client = WEBAUTHN_CLIENT_DATA {
            dwVersion: WEBAUTHN_CLIENT_DATA_CURRENT_VERSION,
            cbClientDataJSON: len(&message)?,
            pbClientDataJSON: message.as_mut_ptr(),
            pwszHashAlgId: WEBAUTHN_HASH_ALGORITHM_SHA_256,
        };
        let mut handle = request.key_handle.to_vec();
        let mut credential = WEBAUTHN_CREDENTIAL {
            dwVersion: WEBAUTHN_CREDENTIAL_CURRENT_VERSION,
            cbId: len(&handle)?,
            pbId: handle.as_mut_ptr(),
            pwszCredentialType: WEBAUTHN_CREDENTIAL_TYPE_PUBLIC_KEY,
        };
        let options = WEBAUTHN_AUTHENTICATOR_GET_ASSERTION_OPTIONS {
            dwVersion: WEBAUTHN_AUTHENTICATOR_GET_ASSERTION_OPTIONS_VERSION_1,
            dwTimeoutMilliseconds: TIMEOUT_MS,
            CredentialList: WEBAUTHN_CREDENTIALS {
                cCredentials: 1,
                pCredentials: &mut credential,
            },
            dwAuthenticatorAttachment: WEBAUTHN_AUTHENTICATOR_ATTACHMENT_ANY,
            dwUserVerificationRequirement: uv(request.flags & USER_VERIFICATION != 0),
            ..Default::default()
        };
        // SAFETY: every pointer refers to a local that outlives the call;
        // Windows parents its dialog to the window in front.
        let assertion = unsafe {
            WebAuthNAuthenticatorGetAssertion(
                GetForegroundWindow(),
                PCWSTR(rp.as_ptr()),
                &client,
                Some(&options),
            )
        }
        .map_err(|e| error(&e))?;
        if assertion.is_null() {
            return Err("Windows returned no assertion".into());
        }
        // SAFETY: on success the API returns a valid assertion, freed here
        // after its buffers are copied.
        let out = unsafe {
            let a = &*assertion;
            let out = Assertion {
                authenticator_data: bytes(a.pbAuthenticatorData, a.cbAuthenticatorData),
                signature: bytes(a.pbSignature, a.cbSignature),
            };
            WebAuthNFreeAssertion(assertion);
            out
        };
        Ok(out)
    }

    fn enroll(&self, request: &EnrollRequest<'_>) -> Result<Vec<u8>, String> {
        let rp_id = HSTRING::from(request.application);
        let rp = WEBAUTHN_RP_ENTITY_INFORMATION {
            dwVersion: WEBAUTHN_RP_ENTITY_INFORMATION_CURRENT_VERSION,
            pwszId: PCWSTR(rp_id.as_ptr()),
            pwszName: PCWSTR(rp_id.as_ptr()),
            pwszIcon: PCWSTR::null(),
        };
        // A random user id: Windows Hello keeps one credential per user id,
        // and a second key must not replace the first.
        let mut user_id = [0u8; 32];
        getrandom::fill(&mut user_id).map_err(|e| e.to_string())?;
        let name = HSTRING::from("FeroSSH");
        let user = WEBAUTHN_USER_ENTITY_INFORMATION {
            dwVersion: WEBAUTHN_USER_ENTITY_INFORMATION_CURRENT_VERSION,
            cbId: len(&user_id)?,
            pbId: user_id.as_mut_ptr(),
            pwszName: PCWSTR(name.as_ptr()),
            pwszIcon: PCWSTR::null(),
            pwszDisplayName: PCWSTR(name.as_ptr()),
        };
        let mut param = WEBAUTHN_COSE_CREDENTIAL_PARAMETER {
            dwVersion: WEBAUTHN_COSE_CREDENTIAL_PARAMETER_CURRENT_VERSION,
            pwszCredentialType: WEBAUTHN_CREDENTIAL_TYPE_PUBLIC_KEY,
            lAlg: request.algorithm.cose(),
        };
        let params = WEBAUTHN_COSE_CREDENTIAL_PARAMETERS {
            cCredentialParameters: 1,
            pCredentialParameters: &mut param,
        };
        let mut challenge = request.challenge.to_vec();
        let client = WEBAUTHN_CLIENT_DATA {
            dwVersion: WEBAUTHN_CLIENT_DATA_CURRENT_VERSION,
            cbClientDataJSON: len(&challenge)?,
            pbClientDataJSON: challenge.as_mut_ptr(),
            pwszHashAlgId: WEBAUTHN_HASH_ALGORITHM_SHA_256,
        };
        let options = WEBAUTHN_AUTHENTICATOR_MAKE_CREDENTIAL_OPTIONS {
            dwVersion: WEBAUTHN_AUTHENTICATOR_MAKE_CREDENTIAL_OPTIONS_VERSION_1,
            dwTimeoutMilliseconds: TIMEOUT_MS,
            dwAuthenticatorAttachment: if request.platform {
                WEBAUTHN_AUTHENTICATOR_ATTACHMENT_PLATFORM
            } else {
                WEBAUTHN_AUTHENTICATOR_ATTACHMENT_ANY
            },
            bRequireResidentKey: BOOL(0),
            dwUserVerificationRequirement: uv(request.user_verification),
            dwAttestationConveyancePreference: WEBAUTHN_ATTESTATION_CONVEYANCE_PREFERENCE_NONE,
            ..Default::default()
        };
        // SAFETY: every pointer refers to a local that outlives the call.
        let attestation = unsafe {
            WebAuthNAuthenticatorMakeCredential(
                GetForegroundWindow(),
                &rp,
                &user,
                &params,
                &client,
                Some(&options),
            )
        }
        .map_err(|e| error(&e))?;
        if attestation.is_null() {
            return Err("Windows returned no credential".into());
        }
        // SAFETY: on success the API returns a valid attestation, freed
        // here after its authenticator data is copied.
        let data = unsafe {
            let a = &*attestation;
            let data = bytes(a.pbAuthenticatorData, a.cbAuthenticatorData);
            WebAuthNFreeCredentialAttestation(Some(attestation));
            data
        };
        Ok(data)
    }
}
