# a comment up top
@feat
Feature: Extra things
  Some description.

  Background: setup
    Given I am logged in

  @smoke @one
  Scenario: Fails on status
    When I list the todos
    Then the response status is 201
    And the response status is 200

  Scenario: Undefined and tables
    Given the variables:
      | name  | value |
      | title | héllo |
    When I do something unknown with 3 "things"
    Then the response status is 200

  @slow
  Scenario Outline: Outline <code>
    When I list the todos
    Then the response status is <code>

    @ex
    Examples: codes
      | code |
      | 200  |
      | 500  |

  Scenario: Doc string
    When I list the todos
    Then the response body contains:
      """json
      [{"id": 99}]
      """

  Scenario: Empty

  Rule: A rule
    Background:
      Given the variable "x" is "1"

    Scenario: In rule
      When I run "nope"
      And another undefined step
