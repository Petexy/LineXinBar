pub const PROTO_MASK: u32 = 0x8000_0000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum EMsg {
    Multi = 1,
    /// Server-initiated unified service notification (e.g. `FriendMessagesClient.IncomingMessage#1`).
    /// Demultiplexed by `target_job_name`, NOT by a per-message EMsg.
    ServiceMethod = 146,
    ServiceMethodResponse = 147,
    ServiceMethodCallFromClient = 151,
    ServiceMethodSendToClient = 152,
    ClientHeartBeat = 703,
    ClientChangeStatus = 716,
    ClientGamesPlayed = 742,
    ClientLogOnResponse = 751,
    ClientPersonaState = 766,
    ClientFriendsList = 767,
    ClientAccountInfo = 768,
    ClientLicenseList = 780,
    ClientRequestFriendData = 815,
    ClientGetUserStats = 818,
    ClientGetUserStatsResponse = 819,
    /// Steam's own answer to "does this account own this app?". It mints a
    /// signed ticket only for an owned app, which is what makes it usable as
    /// an entitlement check rather than a claim the machine makes about itself.
    ClientGetAppOwnershipTicket = 857,
    ClientGetAppOwnershipTicketResponse = 858,
    /// The per-depot AES key, without which a downloaded chunk is noise.
    ///
    /// 1202 is `FileXferData`, not this. Sending that instead gets the session
    /// logged off with `InvalidProtocolVer` and the socket closed, which reads
    /// exactly like Steam having retired the message.
    ClientGetDepotDecryptionKey = 5438,
    ClientGetDepotDecryptionKeyResponse = 5439,
    ClientLogon = 5514,
    ClientPICSProductInfoRequest = 8903,
    ClientPICSProductInfoResponse = 8904,
    ClientPICSAccessTokenRequest = 8905,
    ClientPICSAccessTokenResponse = 8906,
    ServiceMethodCallFromClientNonAuthed = 9804,
    ClientHello = 9805,
}

impl EMsg {
    pub fn raw(self) -> u32 {
        self as u32
    }

    pub fn protobuf(self) -> u32 {
        self.raw() | PROTO_MASK
    }
}
