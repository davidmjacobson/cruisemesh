package com.cruisemesh.app.friending

private val FriendLinkRegex = Regex("""CMFRIEND1:\S+""")

fun extractFriendToken(text: String): String =
    FriendLinkRegex.find(text)?.value ?: text.trim()
